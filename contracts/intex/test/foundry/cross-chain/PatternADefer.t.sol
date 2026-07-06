// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Vm} from "forge-std/Vm.sol";

import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {ITargetMessenger} from "@contracts/target/interfaces/ITargetMessenger.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @notice Stub Auction that synthesises `bidCount` revealed bids (default 1) on `getAuctionDetails`.
///         Used by the TM bids-relay tests to drive `_doSendBidsToOutbe`'s chunked send loop and the
///         defer/flush path. `bidCount = 0` exercises the no-bid → single empty final batch path.
contract StubAuctionWithBids {
    uint256 public bidCount = 1;

    function setBidCount(uint256 n) external {
        bidCount = n;
    }

    function auctionStart(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external {}
    function startRevealingBidsStage(uint32, bool) external {}
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
                timestamp: uint32(block.timestamp)
            });
        }
        // `data` left default — TM's `_doSendBidsToOutbe` drops the first tuple component.
        data;
    }
}

/// @title PatternADeferTest
/// @notice Behavioural coverage of Pattern A on `TargetMessenger`: the inbound clearing/mark-called handlers fire an
///         outbound relay (bids batch / holders bridge) that parks on failure and is retried permissionlessly via
///         `flushPending*`. Failure is forced by starving the relay float — a positive bridge fee with a zero native
///         balance makes `_send` revert `NotEnoughNative`; topping the float up lets the flush land.
contract PatternADeferTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    /// @dev Fee the loopback bridge charges; the relay must have this in native float to send.
    uint256 internal constant BRIDGE_FEE = 0.001 ether;

    TargetMessenger internal bnbMessenger;
    ONFT1155AdapterBatch internal onftBatch;
    ONFT1155AdapterBatch internal onftBatchOutbe;
    IntexNFT1155 internal intex;
    IntexNFT1155 internal intexOutbe;
    StubAuctionWithBids internal stubAuction;

    address internal admin = address(this);
    // Registered peer standing in for the Outbe-side messenger; delivery is authenticated against this address.
    address internal outbePeer = makeAddr("outbePeer");
    uint32 internal constant SERIES_ID = 20260301;
    uint256 internal constant TOKEN_ID = uint256(SERIES_ID);

    function setUp() public {
        _setUpBridge();
        // A positive fee with an unfunded relay float is what forces the inbound-triggered relays to defer.
        bridge.setFee(BRIDGE_FEE);

        intex = DeployProxy.intexNFT1155(admin, admin);
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);

        bnbMessenger = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);
        onftBatch = DeployProxy.onftAdapterBatch(address(intex), address(bridge), admin);
        onftBatchOutbe = DeployProxy.onftAdapterBatch(address(intexOutbe), address(bridge), admin);

        // Register remote messengers so inbound authentication passes and the outbound relay has a destination.
        bnbMessenger.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, outbePeer));
        onftBatch.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(onftBatchOutbe)));

        stubAuction = new StubAuctionWithBids();
        bnbMessenger.wire(address(stubAuction), address(intex), admin, address(onftBatch));

        // Holders bridge: the messenger drives the adapter's systemMultiSend, which crosschainBurns on the local
        // Intex. crosschainBurn is gated by RELAYER_ROLE, and by SYSTEM_RELAYER_ROLE during the Called window.
        onftBatch.grantRole(onftBatch.SYSTEM_RELAYER_ROLE(), address(bnbMessenger));
        intex.grantRole(intex.RELAYER_ROLE(), address(onftBatch));
        intex.grantRole(intex.SYSTEM_RELAYER_ROLE(), address(onftBatch));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbMessenger));

        // Series so markCalled + holder enumeration work.
        intex.createSeries(CreateSeriesLib.params(SERIES_ID, 10_000, 0));
        intex.markQualified(SERIES_ID);
    }

    /// @dev Deliver an inbound packet to the messenger from the registered Outbe peer.
    function _deliverBridge(bytes memory message) internal {
        _deliver(OUTBE_CHAIN_ID, outbePeer, address(bnbMessenger), message);
    }

    // ---------------------------------------------------------------
    // TargetMessenger — bids relay defer + flush
    // ---------------------------------------------------------------

    function test_TM_BidsRelayDeferredOnInsufficientBalance() public {
        // TM has zero native float but the bridge charges a fee, so `_send` reverts when relaying bids.
        assertEq(address(bnbMessenger).balance, 0);

        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));

        // First parked slot.
        (uint32 seriesId, bool exists, bool done) = bnbMessenger.pendingBidsRelays(0);
        assertEq(seriesId, SERIES_ID, "deferred seriesId");
        assertTrue(exists);
        assertFalse(done);
        assertEq(bnbMessenger.nextPendingBidsRelayIdx(), 1);
    }

    function test_TM_FlushBidsRelaySucceedsAfterTopUp() public {
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));

        // Top up TM float generously so the retry can pay the bridge fee.
        vm.deal(address(bnbMessenger), 10 ether);

        bnbMessenger.flushPendingBidsRelay(0);

        (,, bool done) = bnbMessenger.pendingBidsRelays(0);
        assertTrue(done, "flushed slot marked done");
    }

    function test_TM_FlushBidsRelayDoubleFlushRevertsAlreadyFlushed() public {
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));
        vm.deal(address(bnbMessenger), 10 ether);
        bnbMessenger.flushPendingBidsRelay(0);

        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.AlreadyFlushed.selector, 0));
        bnbMessenger.flushPendingBidsRelay(0);
    }

    function test_TM_FlushBidsRelayUnknownIdxReverts() public {
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.NoSuchPendingBidsRelay.selector, 42));
        bnbMessenger.flushPendingBidsRelay(42);
    }

    function test_TM_RelayBidsToOutbe_ExternalCallerRevertsNotSelf() public {
        vm.expectRevert(ITargetMessenger.NotSelf.selector);
        bnbMessenger.relayBidsToOutbe(SERIES_ID);
    }

    // a zero-bid auction still emits one empty final batch (the no-bid completion signal),
    // instead of the old early-return that sent nothing.
    function test_TM_BidsRelay_ZeroBids_SendsOneEmptyFinalBatch() public {
        stubAuction.setBidCount(0);
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));
        vm.deal(address(bnbMessenger), 10 ether);

        vm.recordLogs();
        bnbMessenger.flushPendingBidsRelay(0);
        uint256[] memory sizes = _bidsBatchSentSizes(vm.getRecordedLogs());

        assertEq(sizes.length, 1, "exactly one batch even with no bids");
        assertEq(sizes[0], 0, "the batch is empty");
    }

    // a reveal set larger than MAX_PAYLOAD_ARRAY_LEN is split into multiple batches; the
    // final chunk carries the remainder. (130 bids -> 64 + 64 + 2.)
    function test_TM_BidsRelay_ChunksAboveCap() public {
        stubAuction.setBidCount(130);
        _deliverBridge(BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));
        vm.deal(address(bnbMessenger), 10 ether);

        vm.recordLogs();
        bnbMessenger.flushPendingBidsRelay(0);
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

    // ---------------------------------------------------------------
    // TargetMessenger — holders relay defer + flush
    // ---------------------------------------------------------------

    function _markCalledPacket() internal pure returns (bytes memory) {
        return BridgeMsgCodec.encodeMarkCalled(SERIES_ID);
    }

    function test_TM_HoldersRelayDeferredOnMessengerFloatStarved() public {
        // Seed a holder so `getSeriesHoldersWithBalances` returns non-empty arrays.
        // The inbound MARK_CALLED triggers markCalled + the holders bridge.
        intex.mint(address(0xCAFE), 1, SERIES_ID);

        // TargetMessenger's float is unfunded, so forwarding the quoted fee to `systemMultiSend`
        // fails → holders relay deferred.
        assertEq(address(bnbMessenger).balance, 0);

        _deliverBridge(_markCalledPacket());

        // Auto-getter skips the dynamic-array fields (holders, amounts) — returns (tokenId, exists, done).
        (uint256 storedTokenId, bool exists, bool done) = bnbMessenger.pendingHoldersRelays(0);
        assertEq(storedTokenId, TOKEN_ID, "deferred tokenId");
        assertTrue(exists);
        assertFalse(done);
    }

    function test_TM_FlushHoldersRelaySucceedsAfterMessengerTopUp() public {
        intex.mint(address(0xCAFE), 1, SERIES_ID);
        _deliverBridge(_markCalledPacket());

        // TargetMessenger pays the bridge fee, so top up the messenger (not the adapter).
        vm.deal(address(bnbMessenger), 1 ether);

        bnbMessenger.flushPendingHoldersRelay(0);

        (,, bool done) = bnbMessenger.pendingHoldersRelays(0);
        assertTrue(done, "flushed holders slot marked done");
    }

    function test_TM_FlushHoldersRelayUnknownIdxReverts() public {
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.NoSuchPendingHoldersRelay.selector, 99));
        bnbMessenger.flushPendingHoldersRelay(99);
    }

    function test_TM_BridgeSeriesHoldersExt_ExternalCallerRevertsNotSelf() public {
        address[] memory holders = new address[](0);
        uint256[] memory amounts = new uint256[](0);
        vm.expectRevert(ITargetMessenger.NotSelf.selector);
        bnbMessenger.bridgeSeriesHoldersExt(TOKEN_ID, holders, amounts);
    }
}
