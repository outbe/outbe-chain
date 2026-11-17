// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {BidPackLib} from "../helpers/BidPackLib.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {Vm} from "forge-std/Vm.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {ERC7786MessengerBase} from "@contracts/shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";
import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";

/// @dev A target peer whose delivery tries to flush another parked leg of the same router.
contract ReenteringPeer {
    OriginRouter internal immutable ORIGIN;
    uint256 internal immutable IDX;
    bytes4 public innerRevert;

    constructor(OriginRouter origin, uint256 idx) {
        ORIGIN = origin;
        IDX = idx;
    }

    function receiveMessage(bytes32, bytes calldata, bytes calldata) external payable returns (bytes4) {
        try ORIGIN.retryMessageSend(IDX) {}
        catch (bytes memory reason) {
            innerRevert = bytes4(reason);
        }
        return this.receiveMessage.selector;
    }
}

/// @dev Multi-target OriginRouter behavior: registry, broadcast fan-out over the frozen day snapshot, addressed-send
///      membership, per-leg parking + flush, and inbound BIDS_TOTALS. Delivery is off (sends only record).
contract OriginRouterMultiTargetTest is CrossChainTest {
    uint32 internal constant TARGET_A = 3;
    uint32 internal constant TARGET_B = 4;
    uint32 internal constant DAY = 20_260_716;
    bytes32 internal constant STAGE_SENT_SIG = keccak256("IntexAuctionStartSent(bytes32,uint32,uint32)");

    OriginRouter internal origin;
    address internal desis;
    address internal factory = makeAddr("factory");
    address internal peerA = makeAddr("peerA");
    address internal peerB = makeAddr("peerB");
    address internal admin = address(this);

    function setUp() public {
        _setUpBridge();
        origin = DeployProxy.originRouter(address(bridge), admin);
        desis = address(new MockDesis());
        origin.setDependencies(desis, factory);
        origin.registerRemoteMessenger(TARGET_A, _interop(TARGET_A, peerA));
        origin.registerRemoteMessenger(TARGET_B, _interop(TARGET_B, peerB));
        origin.addTarget(TARGET_A);
        origin.addTarget(TARGET_B);
    }

    function _params(uint32 day) internal pure returns (IOriginRouter.IntexAuctionStartParams memory p) {
        p.prices = ReferenceCurrencyPriceLib.one(840, 1, 3);
        p.emissionDay = day;
    }

    function _fireStart(uint32 day) internal {
        vm.prank(desis);
        origin.sendIntexAuctionStart(_params(day));
    }

    /// @dev Count `IntexAuctionStartSent` emissions in the recorded logs (one per fan-out leg).
    function _countStageSent() internal returns (uint256 n) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == STAGE_SENT_SIG) n++;
        }
    }

    // --- Cancel ---
    function test_cancel_goes_to_the_frozen_snapshot_of_the_day() public {
        _fireStart(DAY);
        origin.removeTarget(TARGET_B);

        vm.recordLogs();
        vm.prank(desis);
        origin.sendIntexAuctionCancel(DAY, 1);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 sent;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == IOriginRouter.IntexAuctionCancelSent.selector) {
                assertEq(uint256(logs[i].topics[2]), DAY, "day");
                sent++;
            }
        }
        assertEq(sent, 2, "both chains the day started on, also the removed one");
    }

    function test_cancel_revert_dayNeverStarted() public {
        vm.prank(desis);
        vm.expectRevert(IOriginRouter.NoTargets.selector);
        origin.sendIntexAuctionCancel(DAY, 1);
    }

    function test_cancel_revert_notDesis() public {
        _fireStart(DAY);
        vm.expectRevert();
        origin.sendIntexAuctionCancel(DAY, 1);
    }

    // --- Registry ---
    function test_registry_views() public view {
        assertTrue(origin.isTarget(TARGET_A));
        assertTrue(origin.isTarget(TARGET_B));
        assertEq(origin.targets().length, 2);
    }

    function test_addTarget_revert_noPeer() public {
        vm.expectRevert(abi.encodeWithSelector(ERC7786MessengerBase.RemoteMessengerNotSet.selector, uint32(9)));
        origin.addTarget(9);
    }

    function test_addTarget_revert_duplicate() public {
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.TargetAlreadyRegistered.selector, TARGET_A));
        origin.addTarget(TARGET_A);
    }

    function test_addTarget_revert_aboveSixteen() public {
        for (uint32 chainId = 100; chainId < 114; ++chainId) {
            origin.registerRemoteMessenger(chainId, _interop(chainId, peerA));
            origin.addTarget(chainId);
        }
        assertEq(origin.targets().length, 16);
        origin.registerRemoteMessenger(114, _interop(114, peerA));
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.TooManyTargets.selector, uint256(16)));
        origin.addTarget(114);
    }

    function test_removeTarget_swapPop() public {
        origin.removeTarget(TARGET_A);
        assertFalse(origin.isTarget(TARGET_A));
        assertTrue(origin.isTarget(TARGET_B));
        uint32[] memory t = origin.targets();
        assertEq(t.length, 1);
        assertEq(t[0], TARGET_B); // swap-pop moved B into A's slot
    }

    function test_removeTarget_revert_notRegistered() public {
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.TargetNotRegistered.selector, uint32(9)));
        origin.removeTarget(9);
    }

    // --- Broadcast fan-out + snapshot ---
    function test_stageStart_snapshotsEveryTarget() public {
        _fireStart(DAY);
        uint32[] memory snap = origin.targetsOf(DAY);
        assertEq(snap.length, 2);
        assertEq(snap[0], TARGET_A);
        assertEq(snap[1], TARGET_B);
    }

    function test_stageStart_fansOutToEveryTarget() public {
        vm.recordLogs();
        _fireStart(DAY);
        assertEq(_countStageSent(), 2, "one leg per target");
    }

    function test_stageStart_revert_noTargets() public {
        origin.removeTarget(TARGET_A);
        origin.removeTarget(TARGET_B);
        vm.prank(desis);
        vm.expectRevert(IOriginRouter.NoTargets.selector);
        origin.sendIntexAuctionStart(_params(DAY));
    }

    /// @dev Clearing is now addressed per chain (each round is sized from that chain's own
    ///      history). Thus membership is what the frozen snapshot says. A mid-day removal must
    ///      not close a chain out.
    function test_clearing_addressesTheSnapshot_notLiveRegistry() public {
        _fireStart(DAY);
        origin.removeTarget(TARGET_B); // a mid-day removal must not shrink an in-flight fan-out
        vm.recordLogs();
        vm.prank(desis);
        origin.sendIntexAuctionClearingStart(DAY, TARGET_B, IntexGas.INTEX_AUCTION_CLEARING_START);
        assertEq(_countStageSent(), 1, "the removed target still takes the day's clearing");

        vm.prank(desis);
        vm.expectRevert(IOriginRouter.NoTargets.selector);
        origin.sendIntexAuctionClearingStart(DAY, 4242, IntexGas.INTEX_AUCTION_CLEARING_START);
    }

    /// @dev A round smaller than a round is worth comes back up to the floor. A round above what a
    ///      target chain would accept comes down to the cap.
    function test_clearing_clampsTheAskToWhatARoundIsWorth() public {
        _fireStart(DAY);
        vm.prank(desis);
        origin.sendIntexAuctionClearingStart(DAY, TARGET_A, 1);
        assertEq(_lastGasAttribute(), IntexGas.INTEX_AUCTION_CLEARING_START, "clamped up to the floor");

        vm.prank(desis);
        origin.sendIntexAuctionClearingStart(DAY, TARGET_A, 100_000_000);
        assertEq(_lastGasAttribute(), IntexGas.INTEX_AUCTION_CLEARING_START_MAX, "clamped down to the cap");
    }

    function _lastGasAttribute() internal view returns (uint256) {
        bytes[] memory attrs = bridge.getLastAttributes();
        return abi.decode(_slice(attrs[0]), (uint256));
    }

    function _slice(bytes memory attribute) internal pure returns (bytes memory body) {
        body = new bytes(attribute.length - 4);
        for (uint256 i = 0; i < body.length; ++i) {
            body[i] = attribute[i + 4];
        }
    }

    // --- Addressed-send membership ---
    function test_addressed_membership_enforced() public {
        _fireStart(DAY);
        vm.prank(desis);
        origin.sendIntexAuctionResult(TARGET_A, DAY, 0, 5, 100, 1e6, 0); // in snapshot: ok

        vm.prank(desis);
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.NotSeriesTarget.selector, DAY, uint32(9)));
        origin.sendIntexAuctionResult(9, DAY, 0, 5, 100, 1e6, 0);
    }

    function test_addressed_removedButSnapshotted_stillRoutes() public {
        _fireStart(DAY);
        origin.removeTarget(TARGET_B); // gone from the registry, still in the day's snapshot
        vm.prank(desis);
        origin.sendIntexAuctionResult(TARGET_B, DAY, 0, 5, 100, 1e6, 0); // must not revert
    }

    // --- Per-leg park + flush ---
    function test_leg_parksOnMissingPeer_thenFlush() public {
        origin.registerRemoteMessenger(TARGET_B, ""); // drop B's peer so its leg fails. A still routes
        _fireStart(DAY);

        IOriginRouter.FailedMessageSend memory p = origin.getFailedMessageSend(0);
        assertEq(p.dstChainId, TARGET_B);
        assertEq(p.sent, false);
        assertGt(p.payload.length, 0);

        origin.registerRemoteMessenger(TARGET_B, _interop(TARGET_B, peerB));
        origin.retryMessageSend(0);
        assertTrue(origin.getFailedMessageSend(0).sent);
    }

    function test_flush_rejectsReentry() public {
        origin.registerRemoteMessenger(TARGET_B, "");
        _fireStart(DAY);
        _fireStart(DAY + 1);
        ReenteringPeer peer = new ReenteringPeer(origin, 1);
        origin.registerRemoteMessenger(TARGET_B, _interop(TARGET_B, address(peer)));
        bridge.setAutoDeliver(true);

        origin.retryMessageSend(0);

        assertEq(peer.innerRevert(), bytes4(keccak256("ReentrancyGuardReentrantCall()")), "re-entry rejected");
        assertTrue(origin.getFailedMessageSend(0).sent);
        assertFalse(origin.getFailedMessageSend(1).sent, "the inner flush did not run");
    }

    function test_flush_revert_unknown() public {
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.NoFailedMessageSend.selector, uint256(0)));
        origin.retryMessageSend(0);
    }

    // --- Inbound BIDS_TOTALS ---
    function test_inbound_bidsDone_dispatches() public {
        _fireStart(DAY); // freeze the day's snapshot so TARGET_A is an accepted source
        bytes memory pkt = BridgeMsgCodec.encodeBidsTotals(DAY, TARGET_A, 2, 7);
        vm.expectCall(desis, abi.encodeWithSelector(IDesis.completeIntexAuctionBids.selector));
        _deliver(TARGET_A, peerA, address(origin), pkt);
    }

    /// @dev A target whose relay stopped part way reports the remainder. The origin answers with
    ///      another CLEARING round to that chain alone. Thus a heavy day finishes without a hand.
    function test_inbound_bidsRemaining_sendsAnotherRound() public {
        _fireStart(DAY);
        bytes memory pkt = BridgeMsgCodec.encodeBidsPause(DAY, TARGET_A, 2, 5);

        _deliver(TARGET_A, peerA, address(origin), pkt);

        assertEq(
            uint8(bridge.lastPayload()[1]),
            BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START,
            "the answer is another clearing round"
        );
        assertEq(
            keccak256(bridge.lastRecipient()),
            keccak256(_interop(TARGET_A, peerA)),
            "and it goes only to the chain that asked"
        );
    }

    function test_inbound_bidsRemaining_fromOutbeSendsTheRoundWithoutParking() public {
        uint32 local = uint32(block.chainid);
        address peerLocal = makeAddr("peerLocal");
        origin.registerRemoteMessenger(local, _interop(local, peerLocal));
        origin.addTarget(local);
        _fireStart(DAY);
        uint256 parkedBefore = origin.failedMessageSendCount();

        _deliver(local, peerLocal, address(origin), BridgeMsgCodec.encodeBidsPause(DAY, local, 2, 5));

        assertEq(uint8(bridge.lastPayload()[1]), BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START, "round sent");
        assertEq(keccak256(bridge.lastRecipient()), keccak256(_interop(local, peerLocal)), "to Outbe itself");
        assertEq(origin.failedMessageSendCount(), parkedBefore, "nothing parked");
    }

    /// @dev Once the day's intake has closed (cleared on the fan-in timeout, cancelled), another
    ///      round would have every chunk it produces ignored on arrival. Thus the origin
    ///      acknowledges the report instead.
    function test_inbound_bidsRemaining_ignoreClosedDay() public {
        _fireStart(DAY);
        MockDesis(desis).setAuctionStage(IDesis.AuctionStage.Completed);
        bytes32 key = bytes32((uint256(DAY) << 32) | TARGET_A);
        bytes memory pkt = BridgeMsgCodec.encodeBidsPause(DAY, TARGET_A, 2, 5);

        vm.expectEmit(false, true, false, true, address(origin));
        emit IOriginRouter.MessageIgnored(bytes32(0), TARGET_A, InboundReason.OBSOLETE);
        _deliver(TARGET_A, peerA, address(origin), pkt);
    }

    function test_inbound_bidsRemaining_ignoreNonSnapshotSource() public {
        _fireStart(DAY);
        origin.registerRemoteMessenger(9, _interop(9, address(0x9999)));
        bytes32 key = bytes32((uint256(DAY) << 32) | 9);
        bytes memory pkt = BridgeMsgCodec.encodeBidsPause(DAY, 9, 1, 2);

        vm.expectEmit(false, true, false, true, address(origin));
        emit IOriginRouter.MessageIgnored(bytes32(0), 9, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), pkt);
    }

    function test_inbound_bids_ignoreNonSnapshotSource() public {
        _fireStart(DAY); // snapshot = {TARGET_A, TARGET_B}. Chain 9 is a registered peer but not a target
        origin.registerRemoteMessenger(9, _interop(9, address(0x9999)));
        bytes32 key = bytes32((uint256(DAY) << 32) | 9);
        bytes memory batch = BridgeMsgCodec.encodeBidsPage(DAY, 9, 0, 1, new address[](0), new uint256[](0));
        vm.expectEmit(false, true, false, true, address(origin));
        emit IOriginRouter.MessageIgnored(bytes32(0), 9, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), batch);

        bytes memory done = BridgeMsgCodec.encodeBidsTotals(DAY, 9, 1, 0);
        vm.expectEmit(false, true, false, true, address(origin));
        emit IOriginRouter.MessageIgnored(bytes32(0), 9, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), done);
    }
}
