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

/// @dev Multi-target OriginRouter behavior: registry, broadcast fan-out over the frozen day snapshot, addressed-send
///      membership, per-leg parking + flush, and inbound BIDS_DONE. Delivery is off (sends only record).
contract OriginRouterMultiTargetTest is CrossChainTest {
    uint32 internal constant TARGET_A = 3;
    uint32 internal constant TARGET_B = 4;
    uint32 internal constant DAY = 20_260_716;
    bytes32 internal constant STAGE_SENT_SIG = keccak256("AuctionStageSent(bytes32,uint32,uint8)");

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
        origin.wire(desis, factory);
        origin.setRemoteMessenger(TARGET_A, _interop(TARGET_A, peerA));
        origin.setRemoteMessenger(TARGET_B, _interop(TARGET_B, peerB));
        origin.addTarget(TARGET_A);
        origin.addTarget(TARGET_B);
    }

    function _params(uint32 day) internal pure returns (IOriginRouter.AuctionStageStartParams memory p) {
        p.prices = ReferenceCurrencyPriceLib.one(840, 1, 2, 3);
        p.worldwideDay = day;
        p.dayState = 1;
    }

    function _fireStart(uint32 day) internal {
        vm.prank(desis);
        origin.sendAuctionStageStart(_params(day));
    }

    /// @dev Count `AuctionStageSent` emissions in the recorded logs (one per fan-out leg).
    function _countStageSent() internal returns (uint256 n) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == STAGE_SENT_SIG) n++;
        }
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
        origin.sendAuctionStageStart(_params(DAY));
    }

    /// @dev Clearing is addressed per chain now (each round is sized from that chain's own history), so
    ///      membership is what the frozen snapshot says - a mid-day removal must not close a chain out.
    function test_clearing_addressesTheSnapshot_notLiveRegistry() public {
        _fireStart(DAY);
        origin.removeTarget(TARGET_B); // a mid-day removal must not shrink an in-flight fan-out
        vm.recordLogs();
        vm.prank(desis);
        origin.sendAuctionStageClearing(DAY, TARGET_B, IntexGas.AUCTION_STAGE_CLEARING);
        assertEq(_countStageSent(), 1, "the removed target still takes the day's clearing");

        vm.prank(desis);
        vm.expectRevert(IOriginRouter.NoTargets.selector);
        origin.sendAuctionStageClearing(DAY, 4242, IntexGas.AUCTION_STAGE_CLEARING);
    }

    /// @dev A round smaller than a round is worth comes back up to the floor, and one above what a target
    ///      chain would accept comes down to the cap.
    function test_clearing_clampsTheAskToWhatARoundIsWorth() public {
        _fireStart(DAY);
        vm.prank(desis);
        origin.sendAuctionStageClearing(DAY, TARGET_A, 1);
        assertEq(_lastGasAttribute(), IntexGas.AUCTION_STAGE_CLEARING, "clamped up to the floor");

        vm.prank(desis);
        origin.sendAuctionStageClearing(DAY, TARGET_A, 100_000_000);
        assertEq(_lastGasAttribute(), IntexGas.AUCTION_STAGE_CLEARING_MAX, "clamped down to the cap");
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
        origin.sendAuctionResult(TARGET_A, DAY, 100, 1e6, 5); // in snapshot: ok

        vm.prank(desis);
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.NotSeriesTarget.selector, DAY, uint32(9)));
        origin.sendAuctionResult(9, DAY, 100, 1e6, 5);
    }

    function test_addressed_removedButSnapshotted_stillRoutes() public {
        _fireStart(DAY);
        origin.removeTarget(TARGET_B); // gone from the registry, still in the day's snapshot
        vm.prank(desis);
        origin.sendAuctionResult(TARGET_B, DAY, 100, 1e6, 5); // must not revert
    }

    // --- Per-leg park + flush ---
    function test_leg_parksOnMissingPeer_thenFlush() public {
        origin.setRemoteMessenger(TARGET_B, ""); // drop B's peer so its leg fails; A still routes
        _fireStart(DAY);

        IOriginRouter.ParkedMessage memory p = origin.parkedMessage(0);
        assertEq(p.dstChainId, TARGET_B);
        assertEq(p.sent, false);
        assertGt(p.payload.length, 0);

        origin.setRemoteMessenger(TARGET_B, _interop(TARGET_B, peerB));
        origin.resendParkedMessage(0);
        assertTrue(origin.parkedMessage(0).sent);
    }

    function test_flush_revert_unknown() public {
        vm.expectRevert(abi.encodeWithSelector(IOriginRouter.NoParkedMessage.selector, uint256(0)));
        origin.resendParkedMessage(0);
    }

    // --- Inbound BIDS_DONE ---
    function test_inbound_bidsDone_dispatches() public {
        _fireStart(DAY); // freeze the day's snapshot so TARGET_A is an accepted source
        bytes memory pkt = BridgeMsgCodec.encodeBidsDone(DAY, TARGET_A, 1, 2, 7);
        vm.expectEmit(true, true, false, true, address(origin));
        emit IOriginRouter.BidsDoneReceived(TARGET_A, DAY, 2, 7);
        _deliver(TARGET_A, peerA, address(origin), pkt);
    }

    /// @dev A target whose relay stopped part way reports the remainder; the origin answers with another
    ///      CLEARING round to that chain alone, so a heavy day finishes without a hand.
    function test_inbound_bidsRemaining_sendsAnotherRound() public {
        _fireStart(DAY);
        bytes memory pkt = BridgeMsgCodec.encodeBidsRemaining(DAY, TARGET_A, 2, 5);

        _deliver(TARGET_A, peerA, address(origin), pkt);

        assertEq(
            uint8(bridge.lastPayload()[1]),
            BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING,
            "the answer is another clearing round"
        );
        assertEq(
            keccak256(bridge.lastRecipient()),
            keccak256(_interop(TARGET_A, peerA)),
            "and it goes only to the chain that asked"
        );
    }

    /// @dev Once the day's intake has closed - cleared on the fan-in timeout, cancelled - another round
    ///      would have every chunk it produces ignored on arrival, so the report is acknowledged instead.
    function test_inbound_bidsRemaining_ignoreClosedDay() public {
        _fireStart(DAY);
        MockDesis(desis).setAuctionStage(IDesis.AuctionStage.Cleared);
        bytes32 key = bytes32((uint256(DAY) << 32) | TARGET_A);
        bytes memory pkt = BridgeMsgCodec.encodeBidsRemaining(DAY, TARGET_A, 2, 5);

        vm.expectEmit(true, true, true, true, address(origin));
        emit IOriginRouter.InboundMessageIgnored(
            TARGET_A, BridgeMsgCodec.MSG_BIDS_REMAINING, key, InboundReason.OBSOLETE
        );
        _deliver(TARGET_A, peerA, address(origin), pkt);
    }

    function test_inbound_bidsRemaining_ignoreNonSnapshotSource() public {
        _fireStart(DAY);
        origin.setRemoteMessenger(9, _interop(9, address(0x9999)));
        bytes32 key = bytes32((uint256(DAY) << 32) | 9);
        bytes memory pkt = BridgeMsgCodec.encodeBidsRemaining(DAY, 9, 1, 2);

        vm.expectEmit(true, true, true, true, address(origin));
        emit IOriginRouter.InboundMessageIgnored(9, BridgeMsgCodec.MSG_BIDS_REMAINING, key, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), pkt);
    }

    function test_inbound_bids_ignoreNonSnapshotSource() public {
        _fireStart(DAY); // snapshot = {TARGET_A, TARGET_B}; chain 9 is a registered peer but not a target
        origin.setRemoteMessenger(9, _interop(9, address(0x9999)));
        bytes32 key = bytes32((uint256(DAY) << 32) | 9);
        bytes memory batch = BridgeMsgCodec.encodeBidsBatch(DAY, 9, 1, 0, 1, new address[](0), new uint256[](0));
        vm.expectEmit(true, true, true, true, address(origin));
        emit IOriginRouter.InboundMessageIgnored(9, BridgeMsgCodec.MSG_BIDS_BATCH, key, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), batch);

        bytes memory done = BridgeMsgCodec.encodeBidsDone(DAY, 9, 1, 1, 0);
        vm.expectEmit(true, true, true, true, address(origin));
        emit IOriginRouter.InboundMessageIgnored(9, BridgeMsgCodec.MSG_BIDS_DONE, key, InboundReason.NOT_FOUND);
        _deliver(9, address(0x9999), address(origin), done);
    }
}
