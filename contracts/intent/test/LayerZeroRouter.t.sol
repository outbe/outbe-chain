// SPDX-License-Identifier: MIT
pragma solidity ^0.8.25;

import {MessagingFee} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/OApp.sol";

import {BaseTest} from "./BaseTest.sol";
import {LayerZeroRouter} from "../src/router/LayerZeroRouter.sol";
import {RouterMessage} from "../src/libs/RouterMessage.sol";
import {EndpointV2Mock} from "./mocks/MockLayerZeroEndpoint.sol";

/**
 * @title LayerZeroRouterForTest
 * @notice Exposes internal handlers for testing and stores test-side state
 */
contract LayerZeroRouterForTest is LayerZeroRouter {
    uint32[] public refundedMessageOrigin;
    bytes32[] public refundedMessageSender;
    bytes32[] public refundedOrderId;

    bytes32[] public settledOrderId;
    bytes32[] public settledOrderReceiver;
    uint32[] public settledMessageOrigin;
    bytes32[] public settledMessageSender;

    uint32 private immutable _FIXED_LOCAL_DOMAIN;

    constructor(
        address _lzEndpoint,
        address _owner,
        uint32 fixedDomain,
        address _compact,
        bytes12 _lockTag,
        address _escrow,
        address _auction
    ) LayerZeroRouter(_lzEndpoint, _owner, _compact, _lockTag, _escrow, _auction) {
        _FIXED_LOCAL_DOMAIN = fixedDomain;
    }

    function _localDomain() internal view override returns (uint32) {
        return _FIXED_LOCAL_DOMAIN;
    }

    function dispatchSettle(uint32 _originDomain, bytes32[] memory _orderIds, bytes[] memory _ordersFillerData)
        public
        payable
    {
        _dispatchSettle(_originDomain, _orderIds, _ordersFillerData);
    }

    function dispatchRefund(uint32 _originDomain, bytes32[] memory _orderIds) public payable {
        _dispatchRefund(_originDomain, _orderIds);
    }

    function _handleSettleOrder(uint32 _messageOrigin, bytes32 _messageSender, bytes32 _orderId, bytes32 _receiver)
        internal
        override
    {
        settledMessageOrigin.push(_messageOrigin);
        settledMessageSender.push(_messageSender);
        settledOrderId.push(_orderId);
        settledOrderReceiver.push(_receiver);
    }

    function _handleRefundOrder(uint32 _messageOrigin, bytes32 _messageSender, bytes32 _orderId) internal override {
        refundedMessageOrigin.push(_messageOrigin);
        refundedMessageSender.push(_messageSender);
        refundedOrderId.push(_orderId);
    }

    function get7383LocalDomain() public view returns (uint32) {
        return _localDomain();
    }

    function addressToBytes32(address _addr) public pure returns (bytes32) {
        return bytes32(uint256(uint160(_addr)));
    }
}

/**
 * @title LayerZeroRouterTest
 * @notice Test suite for the LayerZeroRouter router using the simplified mock endpoint
 */
contract LayerZeroRouterTest is BaseTest {
    EndpointV2Mock internal originEndpoint;
    EndpointV2Mock internal destinationEndpoint;

    LayerZeroRouterForTest internal originRouter;
    LayerZeroRouterForTest internal destinationRouter;

    bytes32 internal originRouterB32;
    bytes32 internal destinationRouterB32;

    uint32 internal originEid;
    uint32 internal destinationEid;

    address internal owner = makeAddr("owner");
    address internal sender = makeAddr("sender");

    function setUp() public override {
        super.setUp();

        // Mock EIDs (LayerZero endpoint IDs)
        originEid = origin;
        destinationEid = destination;

        // Deploy mock endpoints
        originEndpoint = new EndpointV2Mock(originEid);
        destinationEndpoint = new EndpointV2Mock(destinationEid);

        // Deploy routers using the mocks with fixed local domains
        originRouter = new LayerZeroRouterForTest(
            address(originEndpoint),
            owner,
            origin, // Fixed local domain = 1
            address(1), // dummy compact
            bytes12(uint96(1)),
            address(0), // no escrow
            address(1) // dummy auction (unused in these tests)
        );

        destinationRouter = new LayerZeroRouterForTest(
            address(destinationEndpoint),
            owner,
            destination, // Fixed local domain = 2
            address(1), // dummy compact
            bytes12(uint96(1)),
            address(0), // no escrow
            address(1) // dummy auction (unused in these tests)
        );

        // Convert addresses to bytes32 (LayerZero format)
        originRouterB32 = originRouter.addressToBytes32(address(originRouter));
        destinationRouterB32 = destinationRouter.addressToBytes32(address(destinationRouter));

        // Bind OApps to endpoints
        originEndpoint.setOApp(address(originRouter));
        destinationEndpoint.setOApp(address(destinationRouter));

        // Connect endpoints with each other
        originEndpoint.setRemoteEndpoint(destinationEid, destinationEndpoint);
        destinationEndpoint.setRemoteEndpoint(originEid, originEndpoint);

        // Register peers
        originEndpoint.setPeer(destinationEid, destinationRouterB32);
        destinationEndpoint.setPeer(originEid, originRouterB32);

        // Track balances for BaseTest helpers
        balanceId[address(originRouter)] = 4;
        balanceId[address(destinationRouter)] = 5;

        users.push(address(originRouter));
        users.push(address(destinationRouter));
    }

    modifier enrollRouters() {
        vm.startPrank(owner);

        // Register router-level (domain-based) peers
        originRouter.setPeerWithDomain(destinationEid, destinationRouterB32, destination);
        destinationRouter.setPeerWithDomain(originEid, originRouterB32, origin);

        vm.stopPrank();
        _;
    }

    function test_localDomain() public view {
        assertEq(originRouter.get7383LocalDomain(), origin);
        assertEq(destinationRouter.get7383LocalDomain(), destination);
    }

    function test_setPeerWithDomain() public {
        vm.startPrank(owner);

        originRouter.setPeerWithDomain(destinationEid, destinationRouterB32, destination);

        assertEq(originRouter.domainToEid(destination), destinationEid);
        assertEq(originRouter.eidToDomain(destinationEid), destination);

        vm.stopPrank();
    }

    function test_setDefaultGasLimit() public {
        vm.startPrank(owner);

        uint128 newGasLimit = 300_000;
        originRouter.setDefaultGasLimit(newGasLimit);

        assertEq(originRouter.defaultGasLimit(), newGasLimit);

        vm.stopPrank();
    }

    function test_dispatchSettle_works() public enrollRouters {
        address receiver1 = makeAddr("receiver1");
        address receiver2 = makeAddr("receiver2");

        // local arrays
        bytes32[] memory orderIds = new bytes32[](2);
        orderIds[0] = bytes32("order1");
        orderIds[1] = bytes32("order2");

        bytes[] memory fillerData = new bytes[](2);
        fillerData[0] = abi.encode(receiver1);
        fillerData[1] = abi.encode(receiver2);

        // Quote the message
        bytes memory payload = RouterMessage.encodeSettle(orderIds, fillerData);
        MessagingFee memory fee = originRouter.quote(destination, payload, false);

        deal(kakaroto, 1_000_000);

        vm.prank(kakaroto);
        originRouter.dispatchSettle{value: fee.nativeFee}(destination, orderIds, fillerData);

        // Since the mock delivers synchronously, we inspect destinationRouter directly
        assertEq(destinationRouter.settledMessageOrigin(0), origin);
        assertEq(destinationRouter.settledMessageOrigin(1), origin);

        assertEq(destinationRouter.settledMessageSender(0), originRouterB32);
        assertEq(destinationRouter.settledMessageSender(1), originRouterB32);

        assertEq(destinationRouter.settledOrderId(0), orderIds[0]);
        assertEq(destinationRouter.settledOrderId(1), orderIds[1]);

        assertEq(destinationRouter.settledOrderReceiver(0), destinationRouter.addressToBytes32(receiver1));
        assertEq(destinationRouter.settledOrderReceiver(1), destinationRouter.addressToBytes32(receiver2));
    }

    function test_dispatchRefund_works() public enrollRouters {
        // local orderIds
        bytes32[] memory orderIds = new bytes32[](2);
        orderIds[0] = bytes32("id1");
        orderIds[1] = bytes32("id2");

        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = originRouter.quote(destination, payload, false);

        deal(kakaroto, 1_000_000);

        vm.prank(kakaroto);
        originRouter.dispatchRefund{value: fee.nativeFee}(destination, orderIds);

        // Delivery is synchronous
        assertEq(destinationRouter.refundedMessageOrigin(0), origin);
        assertEq(destinationRouter.refundedMessageOrigin(1), origin);

        assertEq(destinationRouter.refundedMessageSender(0), originRouterB32);
        assertEq(destinationRouter.refundedMessageSender(1), originRouterB32);

        assertEq(destinationRouter.refundedOrderId(0), orderIds[0]);
        assertEq(destinationRouter.refundedOrderId(1), orderIds[1]);
    }

    function test_handle_settle_works() public enrollRouters {
        address receiver1 = makeAddr("receiver1");
        address receiver2 = makeAddr("receiver2");

        // local arrays
        bytes32[] memory orderIds = new bytes32[](2);
        orderIds[0] = bytes32("A");
        orderIds[1] = bytes32("B");

        bytes[] memory fillerData = new bytes[](2);
        fillerData[0] = abi.encode(receiver1);
        fillerData[1] = abi.encode(receiver2);

        bytes memory payload = RouterMessage.encodeSettle(orderIds, fillerData);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        deal(kakaroto, 1_000_000);

        vm.prank(kakaroto);
        destinationRouter.dispatchSettle{value: fee.nativeFee}(origin, orderIds, fillerData);

        // Message was delivered synchronously into originRouter
        assertEq(originRouter.settledMessageOrigin(0), destination);
        assertEq(originRouter.settledMessageOrigin(1), destination);

        assertEq(originRouter.settledMessageSender(0), destinationRouterB32);
        assertEq(originRouter.settledMessageSender(1), destinationRouterB32);

        assertEq(originRouter.settledOrderId(0), orderIds[0]);
        assertEq(originRouter.settledOrderId(1), orderIds[1]);

        assertEq(originRouter.settledOrderReceiver(0), originRouter.addressToBytes32(receiver1));
        assertEq(originRouter.settledOrderReceiver(1), originRouter.addressToBytes32(receiver2));
    }

    function test_handle_refund_works() public enrollRouters {
        // local orderIds
        bytes32[] memory orderIds = new bytes32[](2);
        orderIds[0] = bytes32("r1");
        orderIds[1] = bytes32("r2");

        bytes memory payload = RouterMessage.encodeRefund(orderIds);
        MessagingFee memory fee = destinationRouter.quote(origin, payload, false);

        deal(kakaroto, 1_000_000);

        vm.prank(kakaroto);
        destinationRouter.dispatchRefund{value: fee.nativeFee}(origin, orderIds);

        assertEq(originRouter.refundedMessageOrigin(0), destination);
        assertEq(originRouter.refundedMessageOrigin(1), destination);

        assertEq(originRouter.refundedMessageSender(0), destinationRouterB32);
        assertEq(originRouter.refundedMessageSender(1), destinationRouterB32);

        assertEq(originRouter.refundedOrderId(0), orderIds[0]);
        assertEq(originRouter.refundedOrderId(1), orderIds[1]);
    }

    function testFuzz_addressConversion(address testAddr) public view {
        bytes32 converted = originRouter.addressToBytes32(testAddr);
        address reconverted = address(uint160(uint256(converted)));
        assertEq(reconverted, testAddr, "Address conversion must be reversible");
    }

    function test_revert_unauthorizedSetPeer() public {
        vm.prank(sender);
        vm.expectRevert();
        originRouter.setPeerWithDomain(destinationEid, destinationRouterB32, destination);
    }

    function test_revert_unauthorizedSetGasLimit() public {
        vm.prank(sender);
        vm.expectRevert();
        originRouter.setDefaultGasLimit(300_000);
    }

    // ========== Same-Chain Dispatch ==========

    function test_sameChain_dispatchSettle() public {
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = bytes32("sc1");

        bytes[] memory fillerData = new bytes[](1);
        fillerData[0] = abi.encode(originRouter.addressToBytes32(kakaroto));

        // originDomain == localDomain → same-chain path, no LZ fee
        originRouter.dispatchSettle(origin, orderIds, fillerData);

        assertEq(originRouter.settledOrderId(0), orderIds[0]);
        assertEq(originRouter.settledMessageOrigin(0), origin);
        assertEq(originRouter.settledMessageSender(0), originRouter.addressToBytes32(address(originRouter)));
    }

    function test_sameChain_dispatchRefund() public {
        bytes32[] memory orderIds = new bytes32[](1);
        orderIds[0] = bytes32("sc1");

        originRouter.dispatchRefund(origin, orderIds);

        assertEq(originRouter.refundedOrderId(0), orderIds[0]);
        assertEq(originRouter.refundedMessageOrigin(0), origin);
        assertEq(originRouter.refundedMessageSender(0), originRouter.addressToBytes32(address(originRouter)));
    }
}
