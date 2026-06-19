// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {MessagingFee, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";

import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {IOriginMessenger} from "@contracts/origin/interfaces/IOriginMessenger.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";
import {ONFT1155BatchMsgCodec} from "@contracts/shared/libs/ONFT1155BatchMsgCodec.sol";
import {IONFT1155AdapterBatch} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @title InboundValidationTest
/// @notice Inbound validation: the messengers drop malformed/unknown payloads (lane advances,
///         InboundMessageDropped emitted); the ONFT adapters reject them with a typed revert.
/// @dev Tests bypass the LayerZero queue and call `lzReceive` directly from the endpoint
///      address. The endpoint-gate + peer table are honored, so the payload is the only
///      thing the test controls — exactly what we want for validation coverage.
contract InboundValidationTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;
    bytes32 internal constant DUMMY_GUID = bytes32(uint256(0xCAFE));

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    ONFT1155Adapter internal onftBnb;
    ONFT1155Adapter internal onftOutbe;
    ONFT1155AdapterBatch internal onftBatchBnb;
    ONFT1155AdapterBatch internal onftBatchOutbe;

    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    address internal desis;
    address internal intexFactory;
    address internal admin = address(this);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        desis = address(new MockDesis());
        intexFactory = makeAddr("factory");
        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbMessenger = DeployProxy.targetMessenger(address(endpoints[BNB_EID]), admin, OUTBE_EID);
        outbeMessenger = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);
        onftBatchBnb = DeployProxy.onftAdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);

        IntexNFT1155 intexOutbe = DeployProxy.intexNFT1155(admin, admin);
        onftBnb = DeployProxy.onftAdapter(address(intex), address(endpoints[BNB_EID]), admin);
        onftOutbe = DeployProxy.onftAdapter(address(intexOutbe), address(endpoints[OUTBE_EID]), admin);
        onftBatchOutbe = DeployProxy.onftAdapterBatch(address(intexOutbe), address(endpoints[OUTBE_EID]), admin);

        // Wire bridge peers
        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);

        address[] memory onfts = new address[](2);
        onfts[0] = address(onftBnb);
        onfts[1] = address(onftOutbe);
        this.wireOApps(onfts);

        address[] memory batches = new address[](2);
        batches[0] = address(onftBatchBnb);
        batches[1] = address(onftBatchOutbe);
        this.wireOApps(batches);

        bnbMessenger.wire(address(auction), address(intex), admin, address(onftBatchBnb));
        outbeMessenger.wire(desis, intexFactory);
    }

    // --- Helpers ---

    function _deliver(address oapp, address endpointAddr, uint32 srcEid, address peer, bytes memory message) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: 1});
        vm.prank(endpointAddr);
        // `lzReceive` is the public OApp entry — endpoint-only, peer-gated, then routes into
        // the contract's internal `_lzReceive` which is what we are exercising here.
        (bool ok, bytes memory data) = oapp.call(
            abi.encodeWithSignature(
                "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
                origin,
                DUMMY_GUID,
                message,
                address(0),
                ""
            )
        );
        if (!ok) {
            // Re-raise the inner revert with its original selector so vm.expectRevert can match it.
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    /// @dev Assert the next `_deliver` drops the message instead of reverting: the ORDERED lane
    ///      advances and `InboundMessageDropped` carries the original revert as `reason`. The event
    ///      signature is identical on both messengers, so either interface reference matches.
    function _expectDropped(address emitter, uint32 srcEid, bytes memory reason) internal {
        vm.expectEmit(true, true, false, true, emitter);
        emit IOriginMessenger.InboundMessageDropped(DUMMY_GUID, srcEid, reason);
    }

    // ---------------------------------------------------------------
    // TargetMessenger — BridgeMsgCodec validation
    // ---------------------------------------------------------------

    function test_TM_TooShortPayload_DroppedInvalidPayloadLength() public {
        // Only the bodyVersion byte — header itself is shorter than 2.
        bytes memory packet = hex"01";
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(BridgeMsgCodec.InvalidPayloadLength.selector, 0, 1, 2)
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_UnknownMsgType_DroppedUnknownMsgType() public {
        // Header is well-formed (version=1, msgType=0xFE) but msgType is not in TM's accepted set.
        bytes memory packet = hex"01FE";
        // Min-length lookup for an unknown msgType returns 0 — assertMinLength passes; the
        // dispatch else-branch raises `UnknownMsgType(0xFE)`, which is caught and dropped.
        _expectDropped(
            address(bnbMessenger), OUTBE_EID, abi.encodeWithSelector(BridgeMsgCodec.UnknownMsgType.selector, 0xFE)
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_ShortMarkCalled_DroppedInvalidPayloadLength() public {
        // MARK_CALLED min length = 6 (bodyVersion + msgType + seriesId(4)). Build a 5-byte packet.
        // bodyVersion=1, msgType=MARK_CALLED(10), seriesId=20250115 truncated to 3 bytes.
        // seriesId truncated to 3 bytes (uint24) to land at 5 bytes total.
        uint24 truncatedSeriesId = 20_250;
        bytes memory packet =
            abi.encodePacked(BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_MARK_CALLED, truncatedSeriesId);
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(BridgeMsgCodec.InvalidPayloadLength.selector, BridgeMsgCodec.MSG_MARK_CALLED, 5, 6)
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_ShortStageReveal_DroppedInvalidPayloadLength() public {
        // STAGE_REVEAL min length = 7. Send 6-byte packet (missing isGreenDay tail byte).
        bytes memory packet =
            abi.encodePacked(BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL, uint32(1));
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector, BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL, 6, 7
            )
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_ShortRefundInstructions_DroppedInvalidPayloadLength() public {
        // REFUND_INSTRUCTIONS carries three ABI-encoded arrays; its minimum (HEADER_LEN + 224)
        // pins the empty-arrays floor. Send one byte under it to trip the per-type length check.
        uint256 minLen = BridgeMsgCodec.MIN_LEN_REFUND_INSTRUCTIONS;
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS, new bytes(minLen - 3)
        );
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector, BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS, minLen - 1, minLen
            )
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_ShortIssuanceInstructions_DroppedInvalidPayloadLength() public {
        // ISSUANCE_INSTRUCTIONS has the largest minimum (HEADER_LEN + 480): a struct with two
        // arrays. One byte under the floor must trip the per-type length check.
        uint256 minLen = BridgeMsgCodec.MIN_LEN_ISSUANCE_INSTRUCTIONS;
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, new bytes(minLen - 3)
        );
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
                minLen - 1,
                minLen
            )
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    function test_TM_RefundArrayLengthMismatch_Dropped() public {
        // REFUND_INSTRUCTIONS with parallel arrays of unequal length must revert a typed error,
        // not panic out-of-bounds inside the ordered lane.
        address[] memory bidders = new address[](2);
        bidders[0] = address(0xB1);
        bidders[1] = address(0xB2);
        uint64[] memory refundedAmounts = new uint64[](1); // mismatch: 1 vs 2
        refundedAmounts[0] = 1;
        uint64[] memory paidAmounts = new uint64[](2);

        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS,
            abi.encode(uint32(42), bidders, refundedAmounts, paidAmounts)
        );
        _expectDropped(
            address(bnbMessenger),
            OUTBE_EID,
            abi.encodeWithSelector(
                BridgeMsgCodec.RefundArrayLengthMismatch.selector, uint256(2), uint256(1), uint256(2)
            )
        );
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), packet);
    }

    // ---------------------------------------------------------------
    // OriginMessenger — body-srcEid cross-check + msgType
    // ---------------------------------------------------------------

    function test_OM_UnknownMsgType_DroppedUnknownMsgType() public {
        // Pick a msgType the codec itself does not know — `minLengthFor` returns 0 so the
        // per-type length assertion is a no-op, and the OM dispatch else-branch raises
        // `UnknownMsgType(0xFE)`.
        bytes memory packet = hex"01FE";
        _expectDropped(
            address(outbeMessenger), BNB_EID, abi.encodeWithSelector(BridgeMsgCodec.UnknownMsgType.selector, 0xFE)
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);
    }

    /// @notice Reverse of the previous test: a msgType that the codec knows but OM does not
    ///         accept (e.g. MARK_CALLED) fails the length assertion first because the codec's
    ///         `minLengthFor(MARK_CALLED)` returns 6, and our 2-byte packet trips
    ///         `InvalidPayloadLength` before the else-branch is reached. This pins the order:
    ///         length is asserted before msgType-set check.
    function test_OM_CodecKnownButHandlerUnknown_DroppedInvalidPayloadLength() public {
        bytes memory packet = hex"010A"; // bodyVersion + MARK_CALLED (10): codec-known, OM doesn't accept
        _expectDropped(
            address(outbeMessenger),
            BNB_EID,
            abi.encodeWithSelector(BridgeMsgCodec.InvalidPayloadLength.selector, BridgeMsgCodec.MSG_MARK_CALLED, 2, 6)
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);
    }

    function test_OM_BodySrcEidMismatch_DroppedSrcEidBodyMismatch() public {
        // Build a well-formed BIDS_BATCH whose body-srcEid (0xDEAD) disagrees with the
        // transport-layer _origin.srcEid (BNB_EID = 1) → SrcEidBodyMismatch.
        bytes memory packet = BridgeMsgCodec.encodeBidsBatch(
            42, 0xDEAD, true, 1, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
        );
        _expectDropped(
            address(outbeMessenger),
            BNB_EID,
            abi.encodeWithSelector(IOriginMessenger.SrcEidBodyMismatch.selector, BNB_EID, 0xDEAD)
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);
    }

    function test_OM_ShortBidsBatch_DroppedInvalidPayloadLength() public {
        // Empty-arrays BIDS_BATCH = HEADER_LEN + 384 = 386 bytes. Send a one-byte-short packet
        // (truncate the last byte of the trailing length word) to trip the per-type minimum-length check.
        bytes memory full = BridgeMsgCodec.encodeBidsBatch(
            42, BNB_EID, true, 1, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
        );
        bytes memory truncated = new bytes(full.length - 1);
        for (uint256 i = 0; i < truncated.length; i++) {
            truncated[i] = full[i];
        }
        _expectDropped(
            address(outbeMessenger),
            BNB_EID,
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_BIDS_BATCH,
                full.length - 1,
                full.length
            )
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), truncated);
    }

    function test_OM_BidsBatchTooLarge_Dropped() public {
        // A batch above MAX_BIDS_BATCH is rejected on decode before it can stall the lane.
        uint256 n = BridgeMsgCodec.MAX_BIDS_BATCH + 1;
        address[] memory bidders = new address[](n);
        uint16[] memory quantities = new uint16[](n);
        uint64[] memory prices = new uint64[](n);
        uint32[] memory timestamps = new uint32[](n);
        for (uint256 i = 0; i < n; i++) {
            bidders[i] = address(uint160(i + 1));
            quantities[i] = 1;
            prices[i] = 1;
            timestamps[i] = 1;
        }
        // Hand-build the over-cap payload: the outbound encoder caps at MAX_PAYLOAD_ARRAY_LEN (64),
        // so encodeBidsBatch can no longer produce an over-cap batch. Such a message can
        // therefore only reach the inbound handler via a trusted-peer bug — exactly the case the
        // inbound BidsBatchTooLarge decode guard exists to reject.
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_BIDS_BATCH,
            abi.encode(uint32(42), BNB_EID, true, uint32(1), bidders, quantities, prices, timestamps)
        );
        _expectDropped(
            address(outbeMessenger),
            BNB_EID,
            abi.encodeWithSelector(BridgeMsgCodec.BidsBatchTooLarge.selector, n, BridgeMsgCodec.MAX_BIDS_BATCH)
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), packet);
    }

    // ---------------------------------------------------------------
    // OriginMessenger.wire() — Desis interface probe
    // ---------------------------------------------------------------

    function test_OM_Wire_EOA_RevertsInvalidDesisInterface() public {
        OriginMessenger fresh = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);
        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.InvalidDesisInterface.selector, address(0xBEEF)));
        fresh.wire(address(0xBEEF), intexFactory);
    }

    function test_OM_Wire_NonIDesisContract_RevertsInvalidDesisInterface() public {
        // IntexAuction is a contract but does not advertise IDesis via ERC-165.
        OriginMessenger fresh = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);
        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.InvalidDesisInterface.selector, address(auction)));
        fresh.wire(address(auction), intexFactory);
    }

    function test_OM_Wire_MockContracts_Succeeds() public {
        OriginMessenger fresh = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);
        address newDesis = address(new MockDesis());
        address newFactory = makeAddr("newFactory");
        fresh.wire(newDesis, newFactory);
        assertEq(fresh.desis(), newDesis);
        assertEq(fresh.intexFactory(), newFactory);
        assertTrue(fresh.hasRole(fresh.DESIS_ROLE(), newDesis));
        assertTrue(fresh.hasRole(fresh.INTEX_FACTORY_ROLE(), newFactory));
    }

    // ---------------------------------------------------------------
    // ONFT1155Adapter — length + address validation
    // ---------------------------------------------------------------

    function test_ONFT_ShortPayload_RevertsInvalidPayloadLength() public {
        // MIN_LEN_TRANSFER = 97; send 96-byte packet (drop the trailing amount byte).
        bytes memory packet = abi.encodePacked(
            ONFT1155MsgCodec.BODY_VERSION_V1,
            bytes32(uint256(uint160(address(0xCAFE)))),
            uint256(1), // tokenId
            uint192(100) // amount truncated from uint256 to 24 bytes (yields 96-byte total)
        );
        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.InvalidPayloadLength.selector, packet.length, 97));
        _deliver(address(onftBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftOutbe), packet);
    }

    function test_ONFT_MalformedSendTo_RevertsMalformedAddress() public {
        // Build a full-length transfer but corrupt the sendTo field with non-zero high bits.
        bytes32 badRecipient = bytes32(uint256(1) << 200); // high bits set
        bytes memory packet = abi.encodePacked(ONFT1155MsgCodec.BODY_VERSION_V1, badRecipient, uint256(1), uint256(100));
        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.MalformedAddress.selector, badRecipient));
        _deliver(address(onftBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftOutbe), packet);
    }

    // ---------------------------------------------------------------
    // ONFT1155AdapterBatch — V2 codec: version + length + size + msgType + address validation
    // (body migrated to abi.encode, version bumped V1 -> V2)
    // ---------------------------------------------------------------

    function test_ONFTBatch_UnknownMsgType_RevertsUnknownMsgType() public {
        // Valid V2 version byte, unknown msgType 0x99 — routing rejects it.
        bytes memory packet = hex"0299";
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.UnknownMsgType.selector, 0x99));
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_StaleV1Version_RevertsUnsupportedBodyVersion() public {
        // A pre-migration V1 packet must fail closed rather than misdecode into a wrong crosschainMint.
        bytes memory packet = _batchV2(address(0xCAFE), 1, 100);
        packet[0] = bytes1(uint8(1)); // downgrade the version byte to stale V1
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.UnsupportedBodyVersion.selector, uint8(1)));
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_ShortHeader_RevertsInvalidPayloadLength() public {
        // A packet shorter than the [version][msgType] header cannot even be routed.
        bytes memory packet = hex"02";
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.InvalidPayloadLength.selector, 1, 2));
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_TruncatedBody_Reverts() public {
        // Valid header but the abi.encode body is truncated — abi.decode rejects it (no misread).
        bytes memory packet =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND, hex"deadbeef");
        vm.expectRevert();
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_OverCap_RevertsBatchTooLarge() public {
        // Inbound decoded array length is capped at MAX_BATCH_SIZE.
        uint256 over = ONFT1155BatchMsgCodec.MAX_BATCH_SIZE + 1;
        uint256[] memory tokenIds = new uint256[](over);
        uint256[] memory amounts = new uint256[](over);
        bytes memory packet = ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: bytes32(uint256(uint160(address(0xCAFE)))), tokenIds: tokenIds, amounts: amounts
            })
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                ONFT1155BatchMsgCodec.BatchTooLarge.selector, over, ONFT1155BatchMsgCodec.MAX_BATCH_SIZE
            )
        );
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_ZeroRecipient_RevertsInvalidReceiver() public {
        // An all-zero recipient passes the high-bit check but is explicitly rejected.
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(0);
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = 1;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
        );
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_MalformedTo_RevertsMalformedAddress() public {
        // A `to` with non-zero high bits is rejected before any crosschainMint (empty item arrays).
        bytes32 badTo = bytes32(uint256(1) << 200);
        bytes memory packet = abi.encodePacked(
            ONFT1155BatchMsgCodec.BODY_VERSION_V2,
            ONFT1155BatchMsgCodec.SEND,
            abi.encode(
                ONFT1155BatchMsgCodec.BatchPayload({to: badTo, tokenIds: new uint256[](0), amounts: new uint256[](0)})
            )
        );
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.MalformedAddress.selector, badTo));
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_MalformedRecipient_RevertsMalformedAddress() public {
        // SEND_MULTI with one malformed recipient (non-zero high bits).
        bytes32 badRecipient = bytes32(uint256(1) << 200);
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = badRecipient;
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = 1;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
        );
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.MalformedAddress.selector, badRecipient));
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_ZeroTo_RevertsInvalidReceiver() public {
        // SEND branch parity to the SEND_MULTI ZeroRecipient test: assertAddress passes for
        // bytes32(0), so the explicit `if (p.to == bytes32(0))` reject is what stops the crosschainMint.
        bytes memory packet = abi.encodePacked(
            ONFT1155BatchMsgCodec.BODY_VERSION_V2,
            ONFT1155BatchMsgCodec.SEND,
            abi.encode(
                ONFT1155BatchMsgCodec.BatchPayload({
                    to: bytes32(0), tokenIds: new uint256[](0), amounts: new uint256[](0)
                })
            )
        );
        vm.expectRevert(IONFT1155AdapterBatch.InvalidReceiver.selector);
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_MultiOverCap_RevertsBatchTooLarge() public {
        // SEND_MULTI cap parity to the SEND OverCap test — decodeMulti rejects oversize arrays
        // before the per-item loop touches any recipient or crosschainMint.
        uint256 over = ONFT1155BatchMsgCodec.MAX_BATCH_SIZE + 1;
        bytes32[] memory recipients = new bytes32[](over);
        uint256[] memory tokenIds = new uint256[](over);
        uint256[] memory amounts = new uint256[](over);
        bytes memory packet = ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                ONFT1155BatchMsgCodec.BatchTooLarge.selector, over, ONFT1155BatchMsgCodec.MAX_BATCH_SIZE
            )
        );
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    function test_ONFTBatch_MultiArrayMismatch_RevertsArrayLengthMismatch() public {
        // SEND_MULTI array-length mismatch propagates the codec revert through _lzReceive.
        // recipients.length != tokenIds.length is enough to trip decodeMulti's guard.
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = bytes32(uint256(uint160(address(0xCAFE))));
        recipients[1] = bytes32(uint256(uint160(address(0xBEEF))));
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = 1;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 10;
        amounts[1] = 20;
        bytes memory packet = abi.encodePacked(
            ONFT1155BatchMsgCodec.BODY_VERSION_V2,
            ONFT1155BatchMsgCodec.SEND_MULTI,
            abi.encode(
                ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
            )
        );
        vm.expectRevert(ONFT1155BatchMsgCodec.ArrayLengthMismatch.selector);
        _deliver(address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), packet);
    }

    /// @dev Build a one-item V2 SEND packet for a single recipient.
    function _batchV2(address to, uint256 tokenId_, uint256 amount_) internal pure returns (bytes memory) {
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = tokenId_;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount_;
        return ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: bytes32(uint256(uint160(to))), tokenIds: tokenIds, amounts: amounts
            })
        );
    }
}
