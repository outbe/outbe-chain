// SPDX-License-Identifier: MIT
pragma solidity ^0.8.25;

import { MessagingFee } from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/OApp.sol";
import { OptionsBuilder } from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/libs/OptionsBuilder.sol";
import { ResetPeriod } from "the-compact/src/types/ResetPeriod.sol";
import { Scope } from "the-compact/src/types/Scope.sol";

import { LayerZeroRouter } from "../src/router/LayerZeroRouter.sol";
import { Auction } from "../src/Auction.sol";
import { SolverAllocator } from "../src/allocators/SolverAllocator.sol";
import { SolverEscrow } from "../src/SolverEscrow.sol";
import { IAuction } from "../src/interfaces/IAuction.sol";
import { ISolverEscrow } from "../src/interfaces/ISolverEscrow.sol";
import { OnchainCrossChainOrder } from "../src/interfaces/OrderTypes.sol";
import { OrderData, OrderEncoder } from "../src/libs/OrderEncoder.sol";
import { RouterMessage } from "../src/libs/RouterMessage.sol";
import { TypeCasts } from "../src/libs/TypeCasts.sol";

import { BaseTest } from "./BaseTest.sol";
import { EndpointV2Mock } from "./mocks/MockLayerZeroEndpoint.sol";
import { MockTheCompact } from "./mocks/MockTheCompact.sol";

event Filled(bytes32 orderId, bytes originData, bytes fillerData);
event Settle(bytes32[] orderIds, bytes[] ordersFillerData);
event Refund(bytes32[] orderIds);
event Refunded(bytes32 orderId, address receiver);

/**
 * @title LayerZeroRouterWithDomain
 * @notice Test wrapper with fixed local domain
 */
contract LayerZeroRouterWithDomain is LayerZeroRouter {
    uint32 private immutable _fixedLocalDomain;

    constructor(
        address _lzEndpoint,
        address _owner,
        uint32 fixedDomain,
        address _compact,
        bytes12 _lockTag,
        address _escrow,
        address _auction
    )
        LayerZeroRouter(_lzEndpoint, _owner, _compact, _lockTag, _escrow, _auction)
    {
        _fixedLocalDomain = fixedDomain;
    }

    function _localDomain() internal view override returns (uint32) {
        return _fixedLocalDomain;
    }
}

/**
 * @title LayerZeroRouterE2E
 * @notice End-to-end tests for LayerZeroRouter contract with full swap flow using EndpointV2Mock
 */
contract LayerZeroRouterE2E is BaseTest {
    using TypeCasts for address;
    using OptionsBuilder for bytes;

    // EIDs for mock endpoints
    uint32 internal aEid = 1; // origin
    uint32 internal bEid = 2; // destination

    // Mock endpoints
    EndpointV2Mock internal originEndpoint;
    EndpointV2Mock internal destinationEndpoint;

    // Mock Compact — holds origin-chain tokens on behalf of originRouter
    MockTheCompact internal mockCompact;

    // Destination escrow stack
    MockTheCompact internal destCompact;
    SolverEscrow internal destEscrow;

    // Routers
    LayerZeroRouterWithDomain internal originRouter;
    LayerZeroRouterWithDomain internal destinationRouter;

    // Auction
    Auction internal auction;

    bytes32 internal originRouterB32;
    bytes32 internal destinationRouterB32;

    address internal owner = address(this);

    bytes32 constant SALT = bytes32(uint256(42));

    function setUp() public virtual override {
        super.setUp();

        // --- Setup mock LayerZero endpoints ---
        originEndpoint = new EndpointV2Mock(aEid);
        destinationEndpoint = new EndpointV2Mock(bEid);

        // Deploy mock Compacts
        mockCompact = new MockTheCompact();
        destCompact = new MockTheCompact();

        // Deploy auction
        auction = new Auction(owner);

        // Deploy destination escrow + router using CREATE2 to resolve circular dependency
        // (Router needs escrow address, escrow needs router as AUTHORIZED_CALLER)
        (destinationRouter, destEscrow) = _deployRouterWithEscrow(destinationEndpoint, destination, destCompact, 1000);

        // Auction is fixed at deploy (immutable); wire the router as its caller.
        auction.setRouter(address(destinationRouter));

        // Solvers approve escrow as ERC6909 operator and deposit collateral (ERC20 + native)
        uint256 collateralDeposit = 1000;
        address[] memory solvers = new address[](3);
        solvers[0] = vegeta;
        solvers[1] = kakaroto;
        solvers[2] = karpincho;

        for (uint256 i = 0; i < solvers.length; i++) {
            vm.startPrank(solvers[i]);
            destCompact.setOperator(address(destEscrow), true);
            outputToken.approve(address(destEscrow), collateralDeposit);
            destEscrow.deposit(address(outputToken), collateralDeposit);
            destEscrow.deposit{ value: collateralDeposit }(address(0), 0);
            vm.stopPrank();
        }

        // Deploy origin router (no escrow)
        originRouter = new LayerZeroRouterWithDomain(
            address(originEndpoint),
            owner,
            origin,
            address(mockCompact),
            bytes12(uint96(1)),
            address(0), // no escrow on origin
            address(auction)
        );

        // Wire up the routers (bytes32 addresses)
        originRouterB32 = TypeCasts.addressToBytes32(address(originRouter));
        destinationRouterB32 = TypeCasts.addressToBytes32(address(destinationRouter));

        // Bind OApps to endpoints
        originEndpoint.setOApp(address(originRouter));
        destinationEndpoint.setOApp(address(destinationRouter));

        // Connect endpoints with each other
        originEndpoint.setRemoteEndpoint(bEid, destinationEndpoint);
        destinationEndpoint.setRemoteEndpoint(aEid, originEndpoint);

        // Register peers at endpoint level
        originEndpoint.setPeer(bEid, destinationRouterB32);
        destinationEndpoint.setPeer(aEid, originRouterB32);

        // Register router-level (domain-based) peers
        originRouter.setPeerWithDomain(bEid, destinationRouterB32, destination);
        destinationRouter.setPeerWithDomain(aEid, originRouterB32, origin);

        // Index 4 tracks the compact (the real source of origin-chain tokens).
        // Both originRouter and mockCompact map to the same index so that
        // existing assertions using balanceId[address(originRouter)] still resolve.
        balanceId[address(mockCompact)] = 4;
        balanceId[address(originRouter)] = 4;
        balanceId[address(destinationRouter)] = 5;

        users.push(address(mockCompact));
        users.push(address(destinationRouter));
    }

    receive() external payable { }

    // ========== Helpers ==========

    function _prepareOrderData() internal view returns (OrderData memory) {
        return OrderData({
            sender: TypeCasts.addressToBytes32(kakaroto),
            recipient: TypeCasts.addressToBytes32(karpincho),
            inputToken: TypeCasts.addressToBytes32(address(inputToken)),
            outputToken: TypeCasts.addressToBytes32(address(outputToken)),
            amountIn: amount,
            amountOut: amount,
            senderNonce: 1,
            originDomain: origin,
            destinationDomain: destination,
            destinationSettler: address(destinationRouter).addressToBytes32(),
            fillDeadline: uint32(block.timestamp + 100),
            data: new bytes(0)
        });
    }

    /// @dev Single-solver full auction: commit → warp → reveal → warp → ended
    function _doFullAuction(address solver, bytes32 orderId, uint256 outputAmount, bytes memory originData) internal {
        vm.prank(solver);
        auction.commit(orderId, keccak256(abi.encode(orderId, outputAmount, SALT)));
        vm.warp(block.timestamp + auction.commitPeriod());
        vm.prank(solver);
        auction.reveal(orderId, outputAmount, SALT, originData);
        vm.warp(block.timestamp + auction.revealPeriod());
    }

    /// @dev Two-solver full auction
    function _doFullAuctionTwo(
        address solver1,
        uint256 amount1,
        address solver2,
        uint256 amount2,
        bytes32 orderId,
        bytes memory originData
    )
        internal
    {
        vm.prank(solver1);
        auction.commit(orderId, keccak256(abi.encode(orderId, amount1, bytes32(uint256(1)))));
        vm.prank(solver2);
        auction.commit(orderId, keccak256(abi.encode(orderId, amount2, bytes32(uint256(2)))));

        vm.warp(block.timestamp + auction.commitPeriod());

        vm.prank(solver1);
        auction.reveal(orderId, amount1, bytes32(uint256(1)), originData);
        vm.prank(solver2);
        auction.reveal(orderId, amount2, bytes32(uint256(2)), originData);

        vm.warp(block.timestamp + auction.revealPeriod());
    }

    /// @dev Deploys a router + escrow pair. Escrow's authorizedCaller is set after router deployment.
    function _deployRouterWithEscrow(
        EndpointV2Mock endpoint,
        uint32 fixedDomain,
        MockTheCompact compact,
        uint256 collateralBps
    )
        internal
        returns (LayerZeroRouterWithDomain router, SolverEscrow esc)
    {
        SolverAllocator alloc = new SolverAllocator(address(compact));
        bytes12 tag = alloc.buildLockTag(Scope.ChainSpecific, ResetPeriod.TenMinutes);

        esc = new SolverEscrow(address(compact), tag, collateralBps);
        alloc.setArbiter(address(esc));

        router = new LayerZeroRouterWithDomain(
            address(endpoint), owner, fixedDomain, address(compact), tag, address(esc), address(auction)
        );

        esc.setAuthorizedCaller(address(router));
    }

    /// @dev Returns the escrow already deployed in setUp (no-op helper for backward compat)
    function _deployEscrowStack() internal view returns (SolverEscrow esc) {
        return destEscrow;
    }

    // ------------------------------------------------------------------------
    // open -> commit/reveal -> fill -> settle (ERC20)
    // ------------------------------------------------------------------------
    function test_open_fill_settle() public {
        // open
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        vm.recordLogs();
        originRouter.open(order);

        bytes32 orderId = OrderEncoder.id(orderData);

        // commit-reveal (vegeta is the solver)
        vm.stopPrank();
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // claim
        destinationRouter.claimOrder(orderId, order.orderData);

        // fill - only winner can fill after claiming
        vm.startPrank(vegeta);
        outputToken.approve(address(destinationRouter), amount);

        uint256[] memory balancesBeforeFill = _balances(outputToken);

        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(vegeta));
        destinationRouter.fill(orderId, order.orderData, fillerData);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());

        uint256[] memory balancesAfterFill = _balances(outputToken);
        assertEq(balancesAfterFill[balanceId[vegeta]], balancesBeforeFill[balanceId[vegeta]] - amount);
        assertEq(balancesAfterFill[balanceId[karpincho]], balancesBeforeFill[balanceId[karpincho]] + amount);

        // settle
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;
        bytes[] memory ordersFillerData = new bytes[](1);
        ordersFillerData[0] = fillerData;

        bytes memory payload = RouterMessage.encodeSettle(orderIds, ordersFillerData);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        uint256[] memory balancesBeforeSettle = _balances(inputToken);

        vm.expectEmit(false, false, false, true, address(destinationRouter));
        emit Settle(orderIds, ordersFillerData);

        destinationRouter.settle{ value: fee.nativeFee }(orderIds);

        vm.stopPrank();

        uint256[] memory balancesAfterSettle = _balances(inputToken);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());
        assertEq(balancesAfterSettle[balanceId[vegeta]], balancesBeforeSettle[balanceId[vegeta]] + amount);
        assertEq(
            balancesAfterSettle[balanceId[address(originRouter)]],
            balancesBeforeSettle[balanceId[address(originRouter)]] - amount
        );
    }

    // ------------------------------------------------------------------------
    // open -> commit/reveal -> fill -> settle (native)
    // ------------------------------------------------------------------------
    function test_native_open_fill_settle() public {
        // open
        OrderData memory orderData = _prepareOrderData();
        orderData.inputToken = TypeCasts.addressToBytes32(address(0));
        orderData.outputToken = TypeCasts.addressToBytes32(address(0));
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        vm.recordLogs();
        originRouter.open{ value: amount }(order);

        bytes32 orderId = OrderEncoder.id(orderData);

        // commit-reveal
        vm.stopPrank();
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // claim
        destinationRouter.claimOrder(orderId, order.orderData);

        // fill
        vm.startPrank(vegeta);

        uint256[] memory balancesBeforeFill = _balances();

        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(vegeta));
        destinationRouter.fill{ value: amount }(orderId, order.orderData, fillerData);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());

        uint256[] memory balancesAfterFill = _balances();
        assertEq(balancesAfterFill[balanceId[vegeta]], balancesBeforeFill[balanceId[vegeta]] - amount);
        assertEq(balancesAfterFill[balanceId[karpincho]], balancesBeforeFill[balanceId[karpincho]] + amount);

        // settle
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;
        bytes[] memory ordersFillerData = new bytes[](1);
        ordersFillerData[0] = fillerData;

        bytes memory payload = RouterMessage.encodeSettle(orderIds, ordersFillerData);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        uint256[] memory balancesBeforeSettle = _balances();

        vm.expectEmit(false, false, false, true, address(destinationRouter));
        emit Settle(orderIds, ordersFillerData);

        destinationRouter.settle{ value: fee.nativeFee }(orderIds);

        vm.stopPrank();

        uint256[] memory balancesAfterSettle = _balances();

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());
        assertEq(
            balancesAfterSettle[balanceId[vegeta]], balancesBeforeSettle[balanceId[vegeta]] + amount - fee.nativeFee
        );
        assertEq(
            balancesAfterSettle[balanceId[address(originRouter)]],
            balancesBeforeSettle[balanceId[address(originRouter)]] - amount
        );
    }

    // ------------------------------------------------------------------------
    // open -> refund (ERC20)
    // ------------------------------------------------------------------------
    function test_open_refund() public {
        // open
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        vm.recordLogs();
        originRouter.open(order);

        bytes32 orderId = OrderEncoder.id(orderData);

        // refund
        vm.warp(orderData.fillDeadline + 1);

        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;

        OnchainCrossChainOrder[] memory orders = new OnchainCrossChainOrder[](1);
        orders[0] = order;

        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        uint256[] memory balancesBeforeRefund = _balances(inputToken);

        vm.expectEmit(false, false, false, true, address(destinationRouter));
        emit Refund(orderIds);

        destinationRouter.refund{ value: fee.nativeFee }(orders);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.UNKNOWN());

        uint256[] memory balancesAfterRefund = _balances(inputToken);

        assertEq(originRouter.orderStatus(orderId), originRouter.REFUNDED());
        assertEq(
            balancesAfterRefund[balanceId[address(originRouter)]],
            balancesBeforeRefund[balanceId[address(originRouter)]] - amount
        );
        assertEq(balancesAfterRefund[balanceId[kakaroto]], balancesBeforeRefund[balanceId[kakaroto]] + amount);
    }

    // ------------------------------------------------------------------------
    // open -> refund (native)
    // ------------------------------------------------------------------------
    function test_native_open_refund() public {
        // open
        OrderData memory orderData = _prepareOrderData();
        orderData.inputToken = TypeCasts.addressToBytes32(address(0));
        orderData.outputToken = TypeCasts.addressToBytes32(address(0));
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        vm.recordLogs();
        originRouter.open{ value: amount }(order);

        bytes32 orderId = OrderEncoder.id(orderData);

        // refund
        vm.warp(orderData.fillDeadline + 1);

        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;

        OnchainCrossChainOrder[] memory orders = new OnchainCrossChainOrder[](1);
        orders[0] = order;

        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        uint256[] memory balancesBeforeRefund = _balances();

        vm.expectEmit(false, false, false, true, address(destinationRouter));
        emit Refund(orderIds);

        destinationRouter.refund{ value: fee.nativeFee }(orders);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.UNKNOWN());

        uint256[] memory balancesAfterRefund = _balances();

        assertEq(originRouter.orderStatus(orderId), originRouter.REFUNDED());
        assertEq(
            balancesAfterRefund[balanceId[address(originRouter)]],
            balancesBeforeRefund[balanceId[address(originRouter)]] - amount
        );
        assertEq(
            balancesAfterRefund[balanceId[kakaroto]], balancesBeforeRefund[balanceId[kakaroto]] + amount - fee.nativeFee
        );
    }

    // ------------------------------------------------------------------------
    // collateral: full flow with escrow deposit → commit/reveal → fill → settle
    // ------------------------------------------------------------------------
    function test_open_fill_settle_withCollateral() public {
        SolverEscrow esc = _deployEscrowStack();

        // Open order on origin
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Vegeta deposits collateral (10% of outputAmount = amount)
        uint256 collateral = (amount * 1000) / 10_000; // 10%
        vm.startPrank(vegeta);
        outputToken.approve(address(esc), collateral);
        esc.deposit(address(outputToken), collateral);
        vm.stopPrank();

        // Commit-reveal
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // Claim
        destinationRouter.claimOrder(orderId, order.orderData);

        // Fill
        vm.startPrank(vegeta);
        outputToken.approve(address(destinationRouter), amount);
        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(vegeta));
        destinationRouter.fill(orderId, order.orderData, fillerData);

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());

        // Settle
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;
        bytes[] memory ordersFillerData = new bytes[](1);
        ordersFillerData[0] = fillerData;

        bytes memory payload = RouterMessage.encodeSettle(orderIds, ordersFillerData);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        destinationRouter.settle{ value: fee.nativeFee }(orderIds);
        vm.stopPrank();

        // Verify settle completed — vegeta received input tokens on origin
        uint256 vegetaInputBalance = inputToken.balanceOf(vegeta);
        assertGt(vegetaInputBalance, 0, "vegeta should receive input tokens after settle");
    }

    // ------------------------------------------------------------------------
    // slashing: claim → expire → refund = collateral slashed
    // ------------------------------------------------------------------------
    function test_claim_expire_refund_slashes() public {
        SolverEscrow esc = _deployEscrowStack();

        uint256 collateral = amount / 10; // 10%
        vm.startPrank(vegeta);
        outputToken.approve(address(esc), collateral);
        esc.deposit(address(outputToken), collateral);
        vm.stopPrank();

        // Open order
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Commit-reveal
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // Claim
        destinationRouter.claimOrder(orderId, order.orderData);

        // Verify collateral is locked
        uint256 lockId = esc.lockId(address(outputToken));
        assertEq(esc.totalLocked(vegeta, lockId), collateral, "collateral should be locked");

        // Warp past fillDeadline without filling → refund triggers slash
        vm.warp(orderData.fillDeadline + 1);

        OnchainCrossChainOrder[] memory orders = new OnchainCrossChainOrder[](1);
        orders[0] = order;

        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;
        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        destinationRouter.refund{ value: fee.nativeFee }(orders);

        // Collateral should be slashed (lock removed)
        assertEq(esc.totalLocked(vegeta, lockId), 0, "lock consumed after slash");
    }

    // ------------------------------------------------------------------------
    // slashing: claim → fill → settle = collateral unlocked
    // ------------------------------------------------------------------------
    function test_claim_fill_settle_unlocks() public {
        SolverEscrow esc = _deployEscrowStack();

        uint256 collateral = amount / 10;
        vm.startPrank(vegeta);
        outputToken.approve(address(esc), collateral);
        esc.deposit(address(outputToken), collateral);
        vm.stopPrank();

        // Open order
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Commit-reveal
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // Claim
        destinationRouter.claimOrder(orderId, order.orderData);

        uint256 lockId = esc.lockId(address(outputToken));
        assertEq(esc.totalLocked(vegeta, lockId), collateral, "collateral locked after claim");

        // Fill
        vm.startPrank(vegeta);
        outputToken.approve(address(destinationRouter), amount);
        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(vegeta));
        destinationRouter.fill(orderId, order.orderData, fillerData);
        vm.stopPrank();

        // Collateral unlocked after fill
        assertEq(esc.totalLocked(vegeta, lockId), 0, "collateral unlocked after fill");

        (uint256 total,, uint256 available) = esc.getBalance(vegeta, address(outputToken));
        assertEq(total, available, "all collateral available after unlock");
    }

    // ------------------------------------------------------------------------
    // no claim → refund = no slash (UNKNOWN status)
    // ------------------------------------------------------------------------
    function test_noClaim_refund_noSlash() public {
        SolverEscrow esc = _deployEscrowStack();

        // Snapshot balance before the flow
        (uint256 totalBefore,, uint256 availableBefore) = esc.getBalance(vegeta, address(outputToken));

        // Open order
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Commit-reveal but NO claim
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // Warp past fillDeadline → refund without slash
        vm.warp(orderData.fillDeadline + 1);

        uint256 lockId = esc.lockId(address(outputToken));
        assertEq(esc.totalLocked(vegeta, lockId), 0, "nothing locked without claim");

        OnchainCrossChainOrder[] memory orders = new OnchainCrossChainOrder[](1);
        orders[0] = order;

        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;
        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        destinationRouter.refund{ value: fee.nativeFee }(orders);

        // Collateral untouched
        (uint256 total,, uint256 available) = esc.getBalance(vegeta, address(outputToken));
        assertEq(total, totalBefore, "collateral untouched");
        assertEq(available, availableBefore, "all available");
    }

    // ------------------------------------------------------------------------
    // claimOrder restarts auction when winner has no collateral
    // ------------------------------------------------------------------------
    function test_claimOrder_restartsAuction_whenNoCollateral() public {
        SolverEscrow esc = _deployEscrowStack();

        // Open order
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Vegeta deposits collateral, commits, then withdraws before reveal
        uint256 collateral = amount; // enough for quote
        vm.startPrank(vegeta);
        outputToken.approve(address(esc), collateral);
        esc.deposit(address(outputToken), collateral);
        vm.stopPrank();

        // Commit-reveal
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        // Withdraw all collateral before claim
        vm.prank(vegeta);
        esc.withdraw(address(outputToken), 0);

        // claimOrder should restart auction (not revert)
        destinationRouter.claimOrder(orderId, order.orderData);

        // Status stays UNKNOWN
        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.UNKNOWN());

        // Auction was restarted — no quotes, new start time
        assertEq(auction.getQuoteCount(orderId), 0, "quotes cleared");
        assertGt(auction.auctionStartedAt(orderId), 0, "restart timestamp set");

        // New solver (kakaroto) deposits collateral and commits in the new round
        vm.startPrank(kakaroto);
        outputToken.approve(address(esc), amount);
        esc.deposit(address(outputToken), amount);
        vm.stopPrank();

        _doFullAuction(kakaroto, orderId, amount, order.orderData);

        assertEq(auction.getQuoteCount(orderId), 1, "new quote accepted");
    }

    // ------------------------------------------------------------------------
    // full restart flow: restart → new winner → claim → fill → settle
    // ------------------------------------------------------------------------
    function test_restartAuction_fullFlow() public {
        SolverEscrow esc = _deployEscrowStack();

        // Open order
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Vegeta deposits, commits/reveals, then withdraws
        uint256 collateral = amount;
        vm.startPrank(vegeta);
        outputToken.approve(address(esc), collateral);
        esc.deposit(address(outputToken), collateral);
        vm.stopPrank();

        _doFullAuction(vegeta, orderId, amount, order.orderData);

        vm.prank(vegeta);
        esc.withdraw(address(outputToken), 0);

        // Trigger restart
        destinationRouter.claimOrder(orderId, order.orderData);
        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.UNKNOWN());

        // Karpincho deposits collateral and commits/reveals in new round
        address newSolver = karpincho;
        uint256 newCollateral = amount;
        vm.startPrank(newSolver);
        outputToken.approve(address(esc), newCollateral);
        esc.deposit(address(outputToken), newCollateral);
        vm.stopPrank();

        _doFullAuction(newSolver, orderId, amount, order.orderData);

        // Claim succeeds with new winner
        destinationRouter.claimOrder(orderId, order.orderData);
        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.CLAIMED());

        // New winner fills
        vm.startPrank(newSolver);
        outputToken.approve(address(destinationRouter), amount);
        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(newSolver));
        destinationRouter.fill(orderId, order.orderData, fillerData);
        vm.stopPrank();

        assertEq(destinationRouter.destinationOrderStatus(orderId), destinationRouter.FILLED());
    }

    // ------------------------------------------------------------------------
    // settle + reward from slashed pool
    // ------------------------------------------------------------------------
    function _deployOriginEscrowWithSlashedPool(uint256 slashAmount) internal returns (SolverEscrow originEsc) {
        // Redeploy origin router with escrow (resolves circular dependency)
        LayerZeroRouterWithDomain newOriginRouter;
        (newOriginRouter, originEsc) = _deployRouterWithEscrow(originEndpoint, origin, mockCompact, 1000);

        // Replace origin router and rewire everything
        originRouter = newOriginRouter;
        originRouterB32 = TypeCasts.addressToBytes32(address(originRouter));
        originEndpoint.setOApp(address(originRouter));
        originEndpoint.setPeer(bEid, destinationRouterB32);
        destinationEndpoint.setPeer(aEid, originRouterB32);
        originRouter.setPeerWithDomain(bEid, destinationRouterB32, destination);

        // A "bad solver" deposits + gets slashed to create reward pool
        address badSolver = makeAddr("badSolver");
        deal(address(inputToken), badSolver, slashAmount);
        vm.prank(badSolver);
        mockCompact.setOperator(address(originEsc), true);

        vm.startPrank(badSolver);
        inputToken.approve(address(originEsc), slashAmount);
        originEsc.deposit(address(inputToken), slashAmount);
        vm.stopPrank();

        vm.startPrank(address(originRouter));
        originEsc.lockCollateral(keccak256("fakeSlash"), badSolver, address(inputToken), slashAmount);
        originEsc.slashCollateral(keccak256("fakeSlash"));
        vm.stopPrank();
    }

    function test_settle_distributesReward_fromSlashedPool() public {
        SolverEscrow originEsc = _deployOriginEscrowWithSlashedPool(10_000);

        // Fund mockCompact so allocatedTransfer can pay out underlying for reward
        deal(address(inputToken), address(mockCompact), 1_000_000);

        // ---- Normal E2E flow: open → commit/reveal → fill → settle ----
        OrderData memory orderData = _prepareOrderData();
        OnchainCrossChainOrder memory order = OnchainCrossChainOrder({
            fillDeadline: orderData.fillDeadline,
            orderDataType: OrderEncoder.orderDataType(),
            orderData: OrderEncoder.encode(orderData)
        });

        vm.startPrank(kakaroto);
        inputToken.approve(address(originRouter), amount);
        originRouter.open(order);
        vm.stopPrank();

        bytes32 orderId = OrderEncoder.id(orderData);

        // Commit-reveal
        _doFullAuction(vegeta, orderId, amount, order.orderData);

        destinationRouter.claimOrder(orderId, order.orderData);

        // Fill
        vm.startPrank(vegeta);
        outputToken.approve(address(destinationRouter), amount);
        bytes memory fillerData = abi.encode(TypeCasts.addressToBytes32(vegeta));
        destinationRouter.fill(orderId, order.orderData, fillerData);

        // Settle
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = orderId;

        bytes memory payload = RouterMessage.encodeSettle(orderIds, new bytes[](1));
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        uint256 vegetaBefore = inputToken.balanceOf(vegeta);
        destinationRouter.settle{ value: fee.nativeFee }(orderIds);
        vm.stopPrank();

        // Verify: vegeta received amountIn + 1.5% reward
        uint256 expectedReward = (amount * 150) / 10_000;
        assertEq(
            inputToken.balanceOf(vegeta) - vegetaBefore, amount + expectedReward, "solver got amountIn + 1.5% reward"
        );

        // Escrow slashed pool decreased
        uint256 lockId = originEsc.lockId(address(inputToken));
        assertEq(mockCompact.balanceOf(address(originEsc), lockId), 10_000 - expectedReward, "slashed pool decreased");
    }
}
