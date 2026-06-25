// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";

/// @dev Exercises the body-version contract on both codecs:
///      - every encoder emits `bodyVersion == BODY_VERSION_V1` at offset 0;
///      - every decoder reverts `UnsupportedBodyVersion(got)` on any other leading byte;
///      - round-trip preserves the version byte alongside the payload.
contract BodyVersionTest is Test {
    using ONFT1155MsgCodec for bytes;

    // --- BridgeMsgCodec: encoder emits version byte ---

    function test_BridgeCodec_AllEncodersEmitVersionV1() public pure {
        bytes memory encoded;

        encoded = BridgeMsgCodec.encodeBidsBatch(
            1, 30101, true, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
        );
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "bidsBatch.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_BIDS_BATCH, "bidsBatch.msgType");

        encoded = BridgeMsgCodec.encodeAuctionStageStart(1, 100, 200, 300, 840, 840, 1e18, 1e6, 2e6, 3e6, 4e6, 5, 6, 7, 1);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "stageStart.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_AUCTION_STAGE_START, "stageStart.msgType");
        assertEq(encoded.length, 76, "stageStart.length"); // 2 header + 74 body

        encoded = BridgeMsgCodec.encodeAuctionStageReveal(1, true);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "stageReveal.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL, "stageReveal.msgType");
        assertEq(encoded.length, 7, "stageReveal.length");

        encoded = BridgeMsgCodec.encodeAuctionStageClearing(1);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "stageClearing.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, "stageClearing.msgType");
        assertEq(encoded.length, 6, "stageClearing.length");

        encoded = BridgeMsgCodec.encodeAuctionResult(1, 7, 5e6, 3);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "auctionResult.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_AUCTION_RESULT, "auctionResult.msgType");
        assertEq(encoded.length, 22, "auctionResult.length");

        encoded = BridgeMsgCodec.encodeMarkCalled(1);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "markCalled.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_MARK_CALLED, "markCalled.msgType");
        assertEq(encoded.length, 6, "markCalled.length");

        encoded = BridgeMsgCodec.encodeRefundInstructions(1, new address[](0), new uint64[](0), new uint64[](0));
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "refund.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS, "refund.msgType");

        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = 1;
        encoded = BridgeMsgCodec.encodeIssuanceInstructions(payload);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "issuance.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, "issuance.msgType");
    }

    // --- BridgeMsgCodec: round-trip ---

    function test_BridgeCodec_AuctionStageStart_RoundTrip() public view {
        bytes memory packet =
            BridgeMsgCodec.encodeAuctionStageStart(42, 100, 200, 300, 9, 10, 1e18, 5e6, 7e6, 11e6, 13e6, 5, 6, 7, 3);
        (
            uint32 seriesId,
            uint32 commitEnd,
            uint32 revealEnd,
            uint32 issuanceEnd,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint128 promisLoadMinor,
            uint32 minBidRate,
            uint64 entryPrice,
            uint64 floor,
            uint64 callPrice,
            uint32 callPeriod,
            uint16 callWindowDays,
            uint16 callThresholdDays,
            uint16 minQty
        ) = this.exposedDecodeAuctionStageStart(packet);

        assertEq(seriesId, 42);
        assertEq(commitEnd, 100);
        assertEq(revealEnd, 200);
        assertEq(issuanceEnd, 300);
        assertEq(issuanceCurrency, 9);
        assertEq(referenceCurrency, 10);
        assertEq(promisLoadMinor, 1e18);
        assertEq(minBidRate, 5e6);
        assertEq(entryPrice, 7e6);
        assertEq(floor, 11e6);
        assertEq(callPrice, 13e6);
        assertEq(callPeriod, 5);
        assertEq(callWindowDays, 6);
        assertEq(callThresholdDays, 7);
        assertEq(minQty, 3);
    }

    function test_BridgeCodec_AuctionStageReveal_RoundTrip() public view {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(42, true);
        (uint32 seriesId, bool isGreenDay) = this.exposedDecodeAuctionStageReveal(packet);
        assertEq(seriesId, 42);
        assertTrue(isGreenDay);
    }

    function test_BridgeCodec_AuctionResult_RoundTrip() public view {
        bytes memory packet = BridgeMsgCodec.encodeAuctionResult(42, 7, 13e6, 5);
        (uint32 seriesId, uint32 issuedCount, uint64 clearingPrice, uint32 wonCount) =
            this.exposedDecodeAuctionResult(packet);
        assertEq(seriesId, 42);
        assertEq(issuedCount, 7);
        assertEq(clearingPrice, 13e6);
        assertEq(wonCount, 5);
    }

    function test_BridgeCodec_MarkCalled_RoundTrip() public view {
        bytes memory packet = BridgeMsgCodec.encodeMarkCalled(42);
        uint32 seriesId = this.exposedDecodeMarkCalled(packet);
        assertEq(seriesId, 42);
    }

    // --- BridgeMsgCodec: unknown version reverts ---

    function test_BridgeCodec_UnknownBodyVersion_AuctionStageStart_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageStart(1, 100, 200, 300, 840, 840, 1e18, 1e6, 2e6, 3e6, 4e6, 5, 6, 7, 1);
        packet[0] = 0xFF;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0xFF));
        this.exposedDecodeAuctionStageStart(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_AuctionStageReveal_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(1, true);
        packet[0] = 0x02;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x02));
        this.exposedDecodeAuctionStageReveal(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_AuctionResult_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionResult(1, 1, 1, 1);
        packet[0] = 0x42;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x42));
        this.exposedDecodeAuctionResult(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_MarkCalled_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeMarkCalled(1);
        packet[0] = 0xAA;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0xAA));
        this.exposedDecodeMarkCalled(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_BidsBatch_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeBidsBatch(
            1, 30101, true, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
        );
        packet[0] = 0x99;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x99));
        this.exposedDecodeBidsBatch(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_AuctionStageClearing_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageClearing(1);
        packet[0] = 0x10;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x10));
        this.exposedDecodeAuctionStageClearing(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_RefundInstructions_Reverts() public {
        bytes memory packet =
            BridgeMsgCodec.encodeRefundInstructions(1, new address[](0), new uint64[](0), new uint64[](0));
        packet[0] = 0x55;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x55));
        this.exposedDecodeRefundInstructions(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_IssuanceInstructions_Reverts() public {
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = 1;
        bytes memory packet = BridgeMsgCodec.encodeIssuanceInstructions(payload);
        packet[0] = 0x77;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x77));
        this.exposedDecodeIssuanceInstructions(packet);
    }

    // --- ONFT1155MsgCodec: encoder + round-trip + revert ---

    function test_ONFT1155Codec_EmitsVersionV1() public view {
        bytes memory empty;
        (bytes memory payload,) = this.exposedONFTEncode(bytes32(uint256(0xDEAD)), 5, 100, empty);
        assertEq(uint8(payload[0]), ONFT1155MsgCodec.BODY_VERSION_V1);
        assertEq(payload.length, 97); // 1 version + 32 + 32 + 32

        bytes memory compose = hex"cafe";
        (payload,) = this.exposedONFTEncode(bytes32(uint256(0xDEAD)), 5, 100, compose);
        assertEq(uint8(payload[0]), ONFT1155MsgCodec.BODY_VERSION_V1);
        // 1 version + 32 + 32 + 32 + 32 (composer addr) + 2 (compose body)
        assertEq(payload.length, 131);
    }

    function test_ONFT1155Codec_RoundTrip() public view {
        bytes memory empty;
        (bytes memory payload,) = this.exposedONFTEncode(bytes32(uint256(uint160(address(0x1234)))), 5, 100, empty);
        assertEq(this.exposedSendTo(payload), bytes32(uint256(uint160(address(0x1234)))));
        assertEq(this.exposedTokenId(payload), 5);
        assertEq(this.exposedAmount(payload), 100);
        assertFalse(this.exposedIsComposed(payload));
    }

    function test_ONFT1155Codec_UnknownBodyVersion_Reverts() public {
        bytes memory empty;
        (bytes memory payload,) = this.exposedONFTEncode(bytes32(uint256(0xDEAD)), 5, 100, empty);
        payload[0] = 0xEE;

        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.UnsupportedBodyVersion.selector, 0xEE));
        this.exposedSendTo(payload);
        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.UnsupportedBodyVersion.selector, 0xEE));
        this.exposedTokenId(payload);
        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.UnsupportedBodyVersion.selector, 0xEE));
        this.exposedAmount(payload);
    }

    // --- BridgeMsgCodec: sibling-array parity + cap on the variable-length decoders ---

    function test_BridgeCodec_DecodeBidsBatch_RejectsArrayLengthMismatch() public {
        // Four parallel arrays with mismatched lengths: indexed in lockstep downstream, so an
        // unequal decode would panic out of bounds inside the ordered lane. Must revert typed.
        // The encoder now reverts on parity mismatch, so the wire payload is hand-built directly —
        // matching the way an oversized REFUND would arrive via a peer compromise or a future
        // encoder change.
        address[] memory bidders = new address[](2);
        uint16[] memory quantities = new uint16[](1); // short
        uint32[] memory rates = new uint32[](2);
        uint32[] memory timestamps = new uint32[](2);
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_BIDS_BATCH,
            abi.encode(uint32(42), uint32(30101), true, uint32(1), bidders, quantities, rates, timestamps)
        );

        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.BidsArrayLengthMismatch.selector, uint256(2), uint256(1), uint256(2), uint256(2)
            )
        );
        this.exposedDecodeBidsBatch(packet);
    }

    function test_BridgeCodec_DecodeIssuance_RejectsArrayLengthMismatch() public {
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = 1;
        payload.recipients = new address[](3);
        payload.quantities = new uint256[](2); // short
        // Hand-build the body so the encoder's new parity check does not intervene.
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, abi.encode(payload)
        );

        vm.expectRevert(
            abi.encodeWithSelector(BridgeMsgCodec.IssuanceArrayLengthMismatch.selector, uint256(3), uint256(2))
        );
        this.exposedDecodeIssuanceInstructions(packet);
    }

    function test_BridgeCodec_DecodeIssuance_RejectsOverCap() public {
        // The outbound encoder caps recipients at MAX_PAYLOAD_ARRAY_LEN, so an over-cap packet
        // cannot be built through it; hand-build the wire body to exercise the inbound decode cap
        // (the trusted-peer-bug path), reading the cap from the constant.
        uint256 n = uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN) + 1;
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = 1;
        payload.recipients = new address[](n);
        payload.quantities = new uint256[](n);
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, abi.encode(payload)
        );

        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.IssuanceBatchTooLarge.selector, n, uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN)
            )
        );
        this.exposedDecodeIssuanceInstructions(packet);
    }

    function test_BridgeCodec_BidsBatch_IsLast_RoundTrips() public view {
        address[] memory bidders = new address[](1);
        bidders[0] = address(0xB1);
        uint16[] memory quantities = new uint16[](1);
        quantities[0] = 7;
        uint32[] memory rates = new uint32[](1);
        rates[0] = 100;
        uint32[] memory timestamps = new uint32[](1);
        timestamps[0] = 42;

        bytes memory lastPacket =
            BridgeMsgCodec.encodeBidsBatch(1, 30101, true, 7, bidders, quantities, rates, timestamps);
        (,, bool isLastTrue, uint32 genTrue,,,,) = this.exposedDecodeBidsBatch(lastPacket);
        assertTrue(isLastTrue, "isLast=true should round-trip");
        assertEq(genTrue, 7, "relayGeneration should round-trip");

        bytes memory midPacket =
            BridgeMsgCodec.encodeBidsBatch(1, 30101, false, 7, bidders, quantities, rates, timestamps);
        (,, bool isLastFalse,,,,,) = this.exposedDecodeBidsBatch(midPacket);
        assertFalse(isLastFalse, "isLast=false should round-trip");
    }

    // --- External wrappers so calldata-slice helpers can be invoked via vm.expectRevert ---

    function exposedDecodeBidsBatch(bytes calldata p)
        external
        pure
        returns (uint32, uint32, bool, uint32, address[] memory, uint16[] memory, uint32[] memory, uint32[] memory)
    {
        return BridgeMsgCodec.decodeBidsBatch(p);
    }

    function exposedDecodeAuctionStageStart(bytes calldata p)
        external
        pure
        returns (
            uint32,
            uint32,
            uint32,
            uint32,
            uint16,
            uint16,
            uint128,
            uint32,
            uint64,
            uint64,
            uint64,
            uint32,
            uint16,
            uint16,
            uint16
        )
    {
        return BridgeMsgCodec.decodeAuctionStageStart(p);
    }

    function exposedDecodeAuctionStageReveal(bytes calldata p) external pure returns (uint32, bool) {
        return BridgeMsgCodec.decodeAuctionStageReveal(p);
    }

    function exposedDecodeAuctionStageClearing(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeAuctionStageClearing(p);
    }

    function exposedDecodeAuctionResult(bytes calldata p) external pure returns (uint32, uint32, uint64, uint32) {
        return BridgeMsgCodec.decodeAuctionResult(p);
    }

    function exposedDecodeMarkCalled(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeMarkCalled(p);
    }

    function exposedDecodeRefundInstructions(bytes calldata p)
        external
        pure
        returns (uint32, address[] memory, uint64[] memory, uint64[] memory)
    {
        return BridgeMsgCodec.decodeRefundInstructions(p);
    }

    function exposedDecodeIssuanceInstructions(bytes calldata p)
        external
        pure
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory)
    {
        return BridgeMsgCodec.decodeIssuanceInstructions(p);
    }

    function exposedONFTEncode(bytes32 to, uint256 tokenId, uint256 amount, bytes calldata composeMsg)
        external
        view
        returns (bytes memory payload, bool hasCompose)
    {
        return ONFT1155MsgCodec.encode(to, tokenId, amount, composeMsg);
    }

    function exposedSendTo(bytes calldata p) external pure returns (bytes32) {
        return ONFT1155MsgCodec.sendTo(p);
    }

    function exposedTokenId(bytes calldata p) external pure returns (uint256) {
        return ONFT1155MsgCodec.tokenId(p);
    }

    function exposedAmount(bytes calldata p) external pure returns (uint256) {
        return ONFT1155MsgCodec.amount(p);
    }

    function exposedIsComposed(bytes calldata p) external pure returns (bool) {
        return ONFT1155MsgCodec.isComposed(p);
    }
}
