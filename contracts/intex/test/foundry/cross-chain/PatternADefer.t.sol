// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Vm} from "forge-std/Vm.sol";
import {Origin, MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {EnforcedOptionParam} from "@layerzerolabs/oapp-evm/oapp/interfaces/IOAppOptionsType3.sol";

import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {ITargetMessenger} from "@contracts/bnb/interfaces/ITargetMessenger.sol";
import {IIntexAuction} from "@contracts/bnb/interfaces/IIntexAuction.sol";
import {
    IONFT1155AdapterBatch,
    BatchSendParam,
    MultiRecipientSendParam
} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";
// Re-import inside contract context not needed; lib usage via `OptionsBuilder for bytes` declared below.
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/// @notice Stub Auction that synthesises `bidCount` revealed bids (default 1) on `getAuctionDetails`.
///         Used by the TM bids-relay tests to drive `_doSendBidsToOutbe`'s chunked send loop and the
///         defer/flush path. `bidCount = 0` exercises the no-bid → single empty `isLast` batch path.
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
                intexBidPrice: 100e6,
                timestamp: uint32(block.timestamp)
            });
        }
        // `data` left default — TM's `_doSendBidsToOutbe` drops the first tuple component.
        data;
    }
}

/// @notice Stub `IONFT1155AdapterBatch` whose `systemMultiSend` and `quoteSystemMultiSend` always
///         revert with a controllable reason. Used by TM holders-relay defer tests.
contract StubBatchAdapterReverter is IONFT1155AdapterBatch {
    error StubBatchRevert();

    function quoteSystemMultiSend(uint256, address[] calldata, uint256[] calldata, uint32, bytes calldata, bool)
        external
        pure
        returns (MessagingFee memory)
    {
        revert StubBatchRevert();
    }

    function systemMultiSend(
        uint256,
        address[] calldata,
        uint256[] calldata,
        uint32,
        bytes calldata,
        MessagingFee calldata
    ) external payable returns (MessagingReceipt memory) {
        revert StubBatchRevert();
    }

    // --- Unused interface methods (revert if anyone calls them in this test) ---
    function quoteBatchSend(BatchSendParam calldata, bool) external pure returns (MessagingFee memory) {
        revert StubBatchRevert();
    }

    function batchSend(BatchSendParam calldata, MessagingFee calldata, address)
        external
        payable
        returns (MessagingReceipt memory)
    {
        revert StubBatchRevert();
    }

    function quoteMultiSend(MultiRecipientSendParam calldata, bool) external pure returns (MessagingFee memory) {
        revert StubBatchRevert();
    }

    function multiSend(MultiRecipientSendParam calldata, MessagingFee calldata, address)
        external
        payable
        returns (MessagingReceipt memory)
    {
        revert StubBatchRevert();
    }

    function sweepNative(address payable, uint256) external pure {
        revert StubBatchRevert();
    }
}

/// @title PatternADeferTest
/// @notice Behavioural coverage Pattern A on `TargetMessenger` (bids relay + holders
///         bridge) and `ONFT1155Adapter` (compose forward). Each inbound handler defers the
///         outbound send on revert and exposes `flushPending*` for permissionless recovery.
contract PatternADeferTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;
    uint32 internal constant THIRD_EID = 3;

    TargetMessenger internal bnbMessenger;
    ONFT1155Adapter internal onftBnb;
    ONFT1155Adapter internal onftOutbe;
    IntexNFT1155 internal intex;
    IntexNFT1155 internal intexOutbe;
    StubAuctionWithBids internal stubAuction;
    StubBatchAdapterReverter internal stubBatch;

    address internal admin = address(this);
    uint32 internal constant SERIES_ID = 20260301;
    uint256 internal constant TOKEN_ID = uint256(SERIES_ID);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(3, LibraryType.UltraLightNode);

        intex = DeployProxy.intexNFT1155(admin, admin);
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);

        bnbMessenger = DeployProxy.targetMessenger(address(endpoints[BNB_EID]), admin, OUTBE_EID);
        onftBnb = DeployProxy.onftAdapter(address(intex), address(endpoints[BNB_EID]), admin, OUTBE_EID);
        onftOutbe = DeployProxy.onftAdapter(address(intexOutbe), address(endpoints[OUTBE_EID]), admin, BNB_EID);

        address[] memory onfts = new address[](2);
        onfts[0] = address(onftBnb);
        onfts[1] = address(onftOutbe);
        this.wireOApps(onfts);

        // Stubs for TM defer scenarios.
        stubAuction = new StubAuctionWithBids();
        stubBatch = new StubBatchAdapterReverter();
        bnbMessenger.wire(address(stubAuction), address(intex), admin, address(stubBatch));

        // Configure enforcedOptions so `_doSendBidsToOutbe` builds a valid LZ options blob during
        // retry. Without this the ULN rejects with `LZ_ULN_InvalidWorkerOptions(0)`.
        bytes memory bidsOptions = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: OUTBE_EID, msgType: BridgeMsgCodec.MSG_BIDS_BATCH, options: bidsOptions});
        bnbMessenger.setEnforcedOptions(params);

        // Series for the ONFT compose path.
        intex.createSeries(SERIES_ID, 10_000, 0);
        intex.markQualified(SERIES_ID);
        intex.grantRole(intex.RELAYER_ROLE(), address(onftBnb));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbMessenger));

        // Wire bridge peer for TM (need OutbeMessenger as peer; minimal stub instance).
        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(0x1234); // placeholder peer — bnbMessenger.setPeer expects bytes32 form
        // The OAppCore.setPeer is called via wireOApps; placeholder needs to be a deployed OApp.
        // We simply skip outbound wiring for TM since defer tests don't need it to land successfully.
        // The defer happens BEFORE the LZ endpoint validates the peer.
        // (left intentionally without `wireOApps` for TM)
    }

    function _deliverBridge(uint64 nonce, bytes32 guid, bytes memory message) internal {
        Origin memory origin =
            Origin({srcEid: OUTBE_EID, sender: bytes32(uint256(uint160(address(0x1234)))), nonce: nonce});
        // Set the peer manually so OAppReceiver's `_getPeerOrRevert` passes.
        bnbMessenger.setPeer(OUTBE_EID, bytes32(uint256(uint160(address(0x1234)))));

        vm.prank(address(endpoints[BNB_EID]));
        (bool ok, bytes memory data) = address(bnbMessenger)
            .call(
                abi.encodeWithSignature(
                    "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
                    origin,
                    guid,
                    message,
                    address(0),
                    ""
                )
            );
        if (!ok) {
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    function _deliverToOnft(uint32 srcEid, address peer, bytes32 guid, bytes memory message) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: 1});
        vm.prank(address(endpoints[BNB_EID]));
        (bool ok, bytes memory data) = address(onftBnb)
            .call(
                abi.encodeWithSignature(
                    "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
                    origin,
                    guid,
                    message,
                    address(0),
                    ""
                )
            );
        if (!ok) {
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    function _onftComposedPacket(address to, uint256 tokenId_, uint256 amount_, bytes memory composeMsg)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(
            ONFT1155MsgCodec.BODY_VERSION_V1,
            bytes32(uint256(uint160(to))),
            tokenId_,
            amount_,
            bytes32(uint256(uint160(address(0xDEADBEEF)))),
            composeMsg
        );
    }

    // ---------------------------------------------------------------
    // TargetMessenger — bids relay defer + flush
    // ---------------------------------------------------------------

    function test_TM_BidsRelayDeferredOnInsufficientBalance() public {
        // TM has zero native balance, so `_lzSend` will revert when relaying bids.
        assertEq(address(bnbMessenger).balance, 0);

        bytes memory packet = BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID);
        _deliverBridge(1, bytes32(uint256(0xD001)), packet);

        // First parked slot.
        (uint32 seriesId, bool exists, bool done) = bnbMessenger.pendingBidsRelays(0);
        assertEq(seriesId, SERIES_ID, "deferred seriesId");
        assertTrue(exists);
        assertFalse(done);
        assertEq(bnbMessenger.nextPendingBidsRelayIdx(), 1);
    }

    function test_TM_FlushBidsRelaySucceedsAfterTopUp() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID);
        _deliverBridge(1, bytes32(uint256(0xD002)), packet);

        // Top up TM balance generously so the retry can pay the LZ fee.
        vm.deal(address(bnbMessenger), 10 ether);

        bnbMessenger.flushPendingBidsRelay(0);

        (,, bool done) = bnbMessenger.pendingBidsRelays(0);
        assertTrue(done, "flushed slot marked done");
    }

    function test_TM_FlushBidsRelayDoubleFlushRevertsAlreadyFlushed() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID);
        _deliverBridge(1, bytes32(uint256(0xD003)), packet);
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
        _deliverBridge(1, bytes32(uint256(0xD010)), BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));
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
        _deliverBridge(1, bytes32(uint256(0xD011)), BridgeMsgCodec.encodeAuctionStageClearing(SERIES_ID));
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

    function test_TM_HoldersRelayDeferredOnBatchAdapterRevert() public {
        // Seed a holder so `getSeriesHoldersWithBalances` returns non-empty arrays.
        // Skip `markCalled` first — that's what the inbound packet triggers.
        intex.mint(address(0xCAFE), 1, SERIES_ID);

        _deliverBridge(1, bytes32(uint256(0xD101)), _markCalledPacket());

        // Stub batch adapter reverted → holders relay deferred.
        // Auto-getter skips the dynamic-array fields (holders, amounts) — returns
        // (uint256 tokenId, bool exists, bool done).
        (uint256 storedTokenId, bool exists, bool done) = bnbMessenger.pendingHoldersRelays(0);
        assertEq(storedTokenId, TOKEN_ID, "deferred tokenId");
        assertTrue(exists);
        assertFalse(done);
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

    // ---------------------------------------------------------------
    // ONFT1155Adapter — compose defer + flush
    // ---------------------------------------------------------------

    function test_ONFT_DeliverCompose_ExternalCallerRevertsNotSelf() public {
        vm.expectRevert(ONFT1155Adapter.NotSelf.selector);
        onftBnb.deliverCompose(address(0xCAFE), bytes32(uint256(1)), "");
    }

    function test_ONFT_FlushPendingCompose_UnknownIdxReverts() public {
        vm.expectRevert(abi.encodeWithSelector(ONFT1155Adapter.NoSuchPendingCompose.selector, 42));
        onftBnb.flushPendingCompose(42);
    }

    function test_ONFT_ComposeDeferredOnDuplicateSendCompose() public {
        // Wire a third srcEid so the same guid can be delivered from a second peer.
        ONFT1155Adapter onftThird =
            DeployProxy.onftAdapter(address(intexOutbe), address(endpoints[THIRD_EID]), admin, BNB_EID);
        address[] memory triple = new address[](2);
        triple[0] = address(onftBnb);
        triple[1] = address(onftThird);
        this.wireOApps(triple);

        address recipient = address(0xCAFE);
        bytes32 guid = bytes32(uint256(0xE001));
        bytes memory packet = _onftComposedPacket(recipient, TOKEN_ID, 1, hex"deadbeef");

        // First delivery: credit + sendCompose succeed.
        _deliverToOnft(OUTBE_EID, address(onftOutbe), guid, packet);

        // Second delivery (different srcEid bypasses processed[srcEid][guid]; same (to, guid)
        // collides on the endpoint's composeQueue → sendCompose reverts → Pattern A parks it.
        uint256 before_ = onftBnb.nextPendingComposeIdx();
        _deliverToOnft(THIRD_EID, address(onftThird), guid, packet);
        assertEq(onftBnb.nextPendingComposeIdx(), before_ + 1, "compose slot enqueued");

        (address to, bytes32 storedGuid,, bool exists, bool done) = onftBnb.pendingComposes(before_);
        assertEq(to, recipient);
        assertEq(storedGuid, guid);
        assertTrue(exists);
        assertFalse(done);
    }
}
