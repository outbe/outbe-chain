// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../utils/CrossChainTest.sol";
import {DeployProxy} from "../utils/DeployProxy.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {IIntexFactory, INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @dev Minimal EscrowAdapter stand-in. `chargeIntexAuctionAsset` returns a configured totalPaid. The mock
///      also exposes `paymentToken`.
contract MockEscrowAdapter {
    IERC20 public paymentToken;
    uint128 public totalPaidToReturn;

    constructor(IERC20 token) {
        paymentToken = token;
    }

    function dayIntexAuctionAsset(uint32) external view returns (IERC20) {
        return paymentToken;
    }

    function setTotalPaid(uint128 v) external {
        totalPaidToReturn = v;
    }

    function chargeIntexAuctionAsset(uint32, address[] calldata, uint16, uint16, uint64, uint128, bool)
        external
        view
        returns (uint128)
    {
        return totalPaidToReturn;
    }

    function auctionStatus(uint32) external pure returns (bool, bool, uint128) {
        return (false, false, 0);
    }
}

/// @dev Records the composed-transfer send. A test can toggle it to revert, to exercise the park
///      path.
contract MockTokenBridge {
    IERC20 public token;
    bool public shouldRevert;
    uint256 public fee = 0.001 ether;

    uint256 public calls;
    uint32 public lastDomain;
    address public lastTo;
    uint256 public lastAmount;
    bytes public lastExtraData;
    uint256 public lastGas;
    uint256 public valueReceived;

    constructor(IERC20 token_) {
        token = token_;
    }

    function setShouldRevert(bool v) external {
        shouldRevert = v;
    }

    function quoteSend(uint32, address, uint256, bytes calldata, uint256) external view returns (uint256) {
        return fee;
    }

    function sendAndCall(uint32 domain, address to, uint256 amount, bytes calldata extraData, uint256 gasLimit)
        external
        payable
        returns (bytes32)
    {
        require(!shouldRevert, "bridge down");
        calls++;
        lastDomain = domain;
        lastTo = to;
        lastAmount = amount;
        lastExtraData = extraData;
        lastGas = gasLimit;
        valueReceived = msg.value;
        return bytes32(uint256(1));
    }
}

contract IntexAuctionTargetRouterProceedsTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant SERIES_ID = 20_260_713;
    uint128 internal constant AMOUNT = 100e6;

    IntexAuctionTargetRouter internal target;
    MockEscrowAdapter internal escrow;
    MockTokenBridge internal tokenBridge;
    MockWCOEN internal wcoen;

    address internal outbeIca = makeAddr("outbeIca");
    address internal bidder = makeAddr("bidder");

    function setUp() public {
        _setUpBridge();
        wcoen = new MockWCOEN();
        escrow = new MockEscrowAdapter(IERC20(address(wcoen)));
        tokenBridge = new MockTokenBridge(IERC20(address(wcoen)));

        target = _targetOn(OUTBE_CHAIN_ID, address(tokenBridge));
        vm.deal(address(target), 10 ether); // relay float for bridge fees
    }

    /// @dev The IntexFactory precompile takes the local hand-over of the proceeds.
    function _mockIntexFactory() internal {
        vm.etch(INTEX_FACTORY, hex"00");
        vm.mockCall(INTEX_FACTORY, abi.encodeWithSelector(IIntexFactory.receiveLocalProceeds.selector), "");
    }

    function _targetOn(uint32 outbeChainId, address erc20Router) internal returns (IntexAuctionTargetRouter router) {
        router = DeployProxy.targetRouter(
            address(bridge),
            address(this),
            DeployProxy.TargetRouterLinks(
                outbeChainId, erc20Router, makeAddr("auction"), makeAddr("intex"), address(escrow), outbeIca
            )
        );
        _wireOutbeIca(router, outbeIca);
    }

    function _winnersPage(uint32 emissionDay, uint16 pageIndex, uint16 totalPages)
        internal
        view
        returns (bytes memory)
    {
        address[] memory winners = new address[](1);
        winners[0] = bidder;
        return BridgeMsgCodec.encodeIntexAuctionWinners(emissionDay, pageIndex, totalPages, 600_000, 1e6, winners, 0, 0);
    }

    function _deliverRefund(uint32 emissionDay) internal {
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(target), _winnersPage(emissionDay, 0, 1));
    }

    function test_erc20Router_is_the_wcoen_token_bridge() public view {
        assertEq(address(target.erc20Router()), address(tokenBridge));
    }

    function test_RevertWhen_Erc20RouterZero() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.ZeroAddress.selector, "erc20Router"));
        new IntexAuctionTargetRouter(
            address(bridge),
            OUTBE_CHAIN_ID,
            address(0),
            makeAddr("auction"),
            makeAddr("intex"),
            address(escrow),
            outbeIca
        );
    }

    function test_proceeds_of_a_page_carry_the_page_index() public {
        escrow.setTotalPaid(AMOUNT);

        _deliverRefund(SERIES_ID);

        assertEq(tokenBridge.calls(), 1);
        assertEq(tokenBridge.lastDomain(), OUTBE_CHAIN_ID);
        assertEq(tokenBridge.lastTo(), INTEX_FACTORY, "the proceeds go to IntexFactory");
        assertEq(tokenBridge.lastAmount(), AMOUNT);
        assertEq(tokenBridge.lastExtraData(), abi.encode(SERIES_ID, uint32(0)));
        assertEq(tokenBridge.lastGas(), IntexGas.PROCEEDS_COMPOSE);
        assertEq(tokenBridge.valueReceived(), tokenBridge.fee());
        // Router approved the bridge to pull the proceeds.
        assertEq(wcoen.allowance(address(target), address(tokenBridge)), AMOUNT);
    }

    function test_RefundInstructions_ZeroPaidSkipsRouting() public {
        escrow.setTotalPaid(0);
        _deliverRefund(SERIES_ID);
        assertEq(tokenBridge.calls(), 0);
    }

    function test_RefundInstructions_ParksOnBridgeFailure() public {
        escrow.setTotalPaid(AMOUNT);
        tokenBridge.setShouldRevert(true);

        _deliverRefund(SERIES_ID);

        // Finalization still settled. The router parked the send instead of rolling back.
        assertEq(tokenBridge.calls(), 0);
        (uint32 s, uint16 chunk, uint128 a, bool exists, bool done) = target.getFailedIntexAuctionProceedsSend(0);
        assertEq(s, SERIES_ID);
        assertEq(chunk, 0, "the parked route keeps its chunk");
        assertEq(a, AMOUNT);
        assertTrue(exists);
        assertFalse(done);
    }

    function test_RefundInstructions_ParksProceedsTheBridgeDoesNotMove() public {
        escrow.setTotalPaid(AMOUNT);
        MockTokenBridge otherBridge = new MockTokenBridge(IERC20(address(new MockWCOEN())));
        target = _targetOn(OUTBE_CHAIN_ID, address(otherBridge));
        vm.deal(address(target), 10 ether);

        _deliverRefund(SERIES_ID);

        assertEq(otherBridge.calls(), 0, "nothing sent in a token the bridge does not move");
        (uint32 s, uint16 chunk, uint128 a, bool exists, bool done) = target.getFailedIntexAuctionProceedsSend(0);
        assertEq(s, SERIES_ID);
        assertEq(chunk, 0, "the parked route keeps its chunk");
        assertEq(a, AMOUNT);
        assertTrue(exists);
        assertFalse(done);
    }

    function test_on_outbe_the_proceeds_of_a_page_go_to_intex_factory_with_no_send_id() public {
        uint32 local = uint32(block.chainid);
        _mockIntexFactory();
        IntexAuctionTargetRouter outbeTarget = _targetOn(local, address(tokenBridge));
        escrow.setTotalPaid(AMOUNT);
        vm.expectCall(INTEX_FACTORY, abi.encodeCall(IIntexFactory.receiveLocalProceeds, (SERIES_ID, 1, AMOUNT)), 1);

        vm.expectEmit(true, true, true, true, address(outbeTarget));
        emit IIntexAuctionTargetRouter.IntexAuctionProceedsSent(bytes32(0), SERIES_ID, 1, AMOUNT);
        _deliver(local, outbeIca, address(outbeTarget), _winnersPage(SERIES_ID, 1, 2));

        assertEq(tokenBridge.calls(), 0, "the Outbe route does not use the token bridge");
        assertEq(wcoen.allowance(address(outbeTarget), INTEX_FACTORY), AMOUNT, "IntexFactory may pull it");
    }

    function test_RefundInstructions_OnOutbeParksProceedsNotInWcoen() public {
        uint32 local = uint32(block.chainid);
        _mockIntexFactory();
        vm.expectCall(INTEX_FACTORY, abi.encodeWithSelector(IIntexFactory.receiveLocalProceeds.selector), 0);
        MockWCOEN otherWcoen = new MockWCOEN();
        IntexAuctionTargetRouter outbeTarget =
            _targetOn(local, address(new MockTokenBridge(IERC20(address(otherWcoen)))));
        escrow.setTotalPaid(AMOUNT);

        vm.expectEmit(true, true, false, true, address(outbeTarget));
        emit IIntexAuctionTargetRouter.IntexAuctionProceedsSendFailed(
            0,
            SERIES_ID,
            0,
            AMOUNT,
            abi.encodeWithSelector(
                IIntexAuctionTargetRouter.ProceedsTokenNotBridged.selector, address(wcoen), address(otherWcoen)
            )
        );
        _deliver(local, outbeIca, address(outbeTarget), _winnersPage(SERIES_ID, 0, 1));

        (uint32 s,, uint128 a, bool exists, bool done) = outbeTarget.getFailedIntexAuctionProceedsSend(0);
        assertEq(s, SERIES_ID);
        assertEq(a, AMOUNT);
        assertTrue(exists);
        assertFalse(done);
    }

    function test_FlushParkedProceeds_Retries() public {
        escrow.setTotalPaid(AMOUNT);
        tokenBridge.setShouldRevert(true);
        _deliverRefund(SERIES_ID);

        tokenBridge.setShouldRevert(false);
        vm.expectEmit(false, true, true, true, address(target));
        emit IIntexAuctionTargetRouter.IntexAuctionProceedsSent(bytes32(0), SERIES_ID, 0, AMOUNT);
        target.retryIntexAuctionProceedsSend(0);

        assertEq(tokenBridge.calls(), 1);
        assertEq(tokenBridge.lastAmount(), AMOUNT);
        (,,,, bool done) = target.getFailedIntexAuctionProceedsSend(0);
        assertTrue(done);
    }

    function test_RevertWhen_FlushAlreadyDone() public {
        escrow.setTotalPaid(AMOUNT);
        tokenBridge.setShouldRevert(true);
        _deliverRefund(SERIES_ID);
        tokenBridge.setShouldRevert(false);
        target.retryIntexAuctionProceedsSend(0);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.AlreadyResolved.selector, uint256(0)));
        target.retryIntexAuctionProceedsSend(0);
    }

    function test_RevertWhen_FlushUnknownIdx() public {
        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuctionTargetRouter.NoSuchFailedIntexAuctionProceedsSend.selector, uint256(99))
        );
        target.retryIntexAuctionProceedsSend(99);
    }
}
