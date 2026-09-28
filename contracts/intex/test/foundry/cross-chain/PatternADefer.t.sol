// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Vm} from "forge-std/Vm.sol";

import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @notice Stub Auction that synthesises `bidCount` revealed bids (default 1) on `getAuctionDetails`.
///         Used by the TM bids-relay tests to drive `_doSendBidsToOutbe`'s chunked send loop and the
///         defer/flush path. `bidCount = 0` exercises the no-bid -> single empty final batch path.
contract StubAuctionWithBids {
    uint256 public bidCount = 1;

    function setBidCount(uint256 n) external {
        bidCount = n;
    }

    function auctionStart(
        uint32,
        IIntexAuction.WorldwideDayState,
        IIntexAuction.AuctionSchedule calldata,
        IIntexAuction.AuctionParams calldata
    ) external {}
    function startClearingStage(uint32) external {}
    function executeAuctionClearing(uint32, uint32, uint64, uint32) external {}

    function getAuctionDetails(uint32)
        external
        view
        returns (IIntexAuction.AuctionData memory data, IIntexAuction.SubmittedBidData[] memory bids)
    {
        bids = new IIntexAuction.SubmittedBidData[](bidCount);
        for (uint256 i = 0; i < bidCount; i++) {
            bids[i] = IIntexAuction.SubmittedBidData({
                bidderAddress: address(uint160(0xCAFE + i)),
                intexQuantity: 1,
                intexBidRate: 100e6,
                timestamp: uint32(block.timestamp),
                issuanceCurrency: 840,
                referenceCurrency: 840
            });
        }
        // `data` left default - TM's `_doSendBidsToOutbe` drops the first tuple component.
        data;
    }

    function revealedBidsCount(uint32) external view returns (uint256) {
        return bidCount;
    }

    function revealedBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedBidData[] memory slice)
    {
        uint256 length = bidCount;
        if (offset >= length) return new IIntexAuction.SubmittedBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = IIntexAuction.SubmittedBidData({
                bidderAddress: address(uint160(0xCAFE + offset + i)),
                intexQuantity: 1,
                intexBidRate: 100e6,
                timestamp: uint32(block.timestamp),
                issuanceCurrency: 840,
                referenceCurrency: 840
            });
        }
    }

    IIntexAuction.AuctionStage public stage = IIntexAuction.AuctionStage.Issuance;

    function setStage(IIntexAuction.AuctionStage s) external {
        stage = s;
    }

    function getAuctionStage(uint32) external view returns (IIntexAuction.AuctionStage) {
        return stage;
    }
}

/// @title PatternADeferTest
/// @notice Behavioural coverage of Pattern A on `TargetRouter`: the inbound clearing/mark-called handlers fire an
///         outbound relay (bids batch / owners bridge) that parks on failure and is retried permissionlessly via
///         `flushPending*`. Failure is forced by starving the relay float - a positive bridge fee with a zero native
///         balance makes `_send` revert `NotEnoughNative`; topping the float up lets the flush land.
contract PatternADeferTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    /// @dev Fee the loopback bridge charges; the relay must have this in native float to send.
    uint256 internal constant BRIDGE_FEE = 0.001 ether;

    TargetRouter internal bnbRouter;
    IntexNFT1155Bridge internal nftBridge;
    IntexNFT1155Bridge internal nftBridgeOutbe;
    IntexNFT1155 internal intex;
    IntexNFT1155 internal intexOutbe;
    StubAuctionWithBids internal stubAuction;

    address internal admin = address(this);
    // Registered peer standing in for the Outbe-side router; delivery is authenticated against this address.
    address internal outbePeer = makeAddr("outbePeer");
    uint32 internal constant SERIES_ID_DAY = 20260301;
    bytes14 internal constant SERIES_ID = "20260301-USD-U";
    uint256 internal constant TOKEN_ID = uint256(uint112(SERIES_ID));

    function setUp() public {
        _setUpBridge();
        // A positive fee with an unfunded relay float is what forces the inbound-triggered relays to defer.
        bridge.setFee(BRIDGE_FEE);

        intex = DeployProxy.intexNFT1155(admin, admin);
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        nftBridge = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);
        nftBridgeOutbe = DeployProxy.intexNFT1155Bridge(address(intexOutbe), address(bridge), admin);

        // Register remote messengers so inbound authentication passes and the outbound relay has a destination.
        bnbRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, outbePeer));
        nftBridge.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftBridgeOutbe)));

        stubAuction = new StubAuctionWithBids();
        bnbRouter.wire(address(stubAuction), address(intex), admin);

        // The bridge burns on the local Intex.
        intex.grantRole(intex.RELAYER_ROLE(), address(nftBridge));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbRouter));

        // Series so markCalled + owner enumeration work.
        intex.createSeries(CreateSeriesLib.params(SERIES_ID_DAY, 10_000, 0));
    }

    /// @dev Deliver an inbound packet to the router from the registered Outbe peer.
    function _deliverBridge(bytes memory message) internal {
        _deliver(OUTBE_CHAIN_ID, outbePeer, address(bnbRouter), message);
    }

    // ---------------------------------------------------------------
    // TargetRouter - bids relay: a round that cannot pay leaves the day resumable
    // ---------------------------------------------------------------

    function test_TM_BidsRelayStopsWhenTheFloatCannotPay() public {
        // TM has zero native float but the bridge charges a fee, so `_send` reverts when relaying bids.
        assertEq(address(bnbRouter).balance, 0);

        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));

        (uint16 nextBatch, uint16 totalBatches, bool done) = bnbRouter.bidsRelay(SERIES_ID_DAY);
        assertEq(nextBatch, 0, "the round rolled back whole");
        assertEq(totalBatches, 0, "including the span it had frozen");
        assertFalse(done, "and the day stays open for the next round");
    }

    function test_TM_RelayBidsFinishesTheDayAfterTopUp() public {
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));

        // Top up TM float generously so the resumed round can pay the bridge fee.
        vm.deal(address(bnbRouter), 10 ether);
        bnbRouter.relayBids(SERIES_ID_DAY);

        (,, bool done) = bnbRouter.bidsRelay(SERIES_ID_DAY);
        assertTrue(done, "the day relayed whole");
    }

    function test_TM_RelayBidsOnAFinishedDayReverts() public {
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));
        vm.deal(address(bnbRouter), 10 ether);
        bnbRouter.relayBids(SERIES_ID_DAY);

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoBidsToRelay.selector, SERIES_ID_DAY));
        bnbRouter.relayBids(SERIES_ID_DAY);
    }

    /// @dev A day the auction has not moved past its reveal is nobody's to relay: that stage is set by the
    ///      inbound CLEARING alone.
    function test_TM_RelayBidsBeforeClearingReverts() public {
        stubAuction.setStage(IIntexAuction.AuctionStage.RevealingBids);

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoBidsToRelay.selector, SERIES_ID_DAY));
        bnbRouter.relayBids(SERIES_ID_DAY);
    }

    /// @dev A round that moved but could not finish reports the remainder home, and the origin answers that
    ///      with another round - so a heavy day needs no hand at all.
    function test_TM_AStoppedRoundReportsWhatIsLeft() public {
        stubAuction.setBidCount(130); // three chunks
        vm.deal(address(bnbRouter), 10 ether);

        // Deliver with enough gas for a chunk or two, not for the day: exactly what a tight budget does.
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY);
        (bool delivered,) = address(bridge).call{gas: 4_500_000}(
            abi.encodeCall(
                bridge.deliverAs,
                (_interop(OUTBE_CHAIN_ID, outbePeer), _interop(uint32(block.chainid), address(bnbRouter)), packet)
            )
        );
        assertTrue(delivered, "the delivery itself must survive");

        (uint16 nextBatch, uint16 totalBatches, bool done) = bnbRouter.bidsRelay(SERIES_ID_DAY);
        assertFalse(done, "the day is unfinished");
        assertGt(nextBatch, 0, "the round moved");
        assertLt(nextBatch, totalBatches, "and left chunks behind");

        bytes memory reported = bridge.lastPayload();
        assertEq(uint8(reported[1]), BridgeMsgCodec.MSG_BIDS_REMAINING, "the last thing sent is the report");
    }

    /// @dev A round that sent nothing must not report: the origin would answer with the same budget for the
    ///      same outcome, and that is a loop rather than a recovery.
    function test_TM_ARoundThatSentNothingDoesNotReport() public {
        // Zero float, so the round reverts whole before any chunk leaves.
        assertEq(address(bnbRouter).balance, 0);
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));

        assertEq(bridge.lastPayload().length, 0, "nothing was sent, so nothing was reported");
    }

    function test_TM_RelayBidsToOutbe_ExternalCallerRevertsNotSelf() public {
        vm.expectRevert(ITargetRouter.NotSelf.selector);
        bnbRouter.relayBidsToOutbe(SERIES_ID_DAY);
    }

    // a zero-bid auction still emits one empty final batch (the no-bid completion signal),
    // instead of the old early-return that sent nothing.
    function test_TM_BidsRelay_ZeroBids_SendsOneEmptyFinalBatch() public {
        stubAuction.setBidCount(0);
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));
        vm.deal(address(bnbRouter), 10 ether);

        vm.recordLogs();
        bnbRouter.relayBids(SERIES_ID_DAY);
        uint256[] memory sizes = _bidsBatchSentSizes(vm.getRecordedLogs());

        assertEq(sizes.length, 1, "exactly one batch even with no bids");
        assertEq(sizes[0], 0, "the batch is empty");
    }

    // a reveal set larger than MAX_PAYLOAD_ARRAY_LEN is split into multiple batches; the
    // final chunk carries the remainder. (130 bids -> 64 + 64 + 2.)
    function test_TM_BidsRelay_ChunksAboveCap() public {
        stubAuction.setBidCount(130);
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID_DAY));
        vm.deal(address(bnbRouter), 10 ether);

        vm.recordLogs();
        bnbRouter.relayBids(SERIES_ID_DAY);
        uint256[] memory sizes = _bidsBatchSentSizes(vm.getRecordedLogs());

        assertEq(sizes.length, 3, "ceil(130 / 64) = 3 chunks");
        assertEq(sizes[0], 64, "chunk 0 at cap");
        assertEq(sizes[1], 64, "chunk 1 at cap");
        assertEq(sizes[2], 2, "chunk 2 remainder");
    }

    /// @dev Extract the `bidsCount` of every `BidsBatchSent` log, in emission order.
    function _bidsBatchSentSizes(Vm.Log[] memory logs) internal pure returns (uint256[] memory sizes) {
        bytes32 topic = keccak256("BidsBatchSent(bytes32,uint32,uint256)");
        uint256 n;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length != 0 && logs[i].topics[0] == topic) n++;
        }
        sizes = new uint256[](n);
        uint256 j;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length != 0 && logs[i].topics[0] == topic) {
                sizes[j++] = abi.decode(logs[i].data, (uint256));
            }
        }
    }
}
