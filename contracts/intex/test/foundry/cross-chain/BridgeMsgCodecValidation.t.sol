// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev PR-A Tier-1 input-validation hardening of BridgeMsgCodec:
///      - fixed-width decoders assert exact length (truncation silent-truncation);
///      - `isGreenDay` decodes strictly;
///      - outbound encoders cap payload arrays at `MAX_PAYLOAD_ARRAY_LEN`;
///      - `decode*` deliberately does NOT cap (inbound is A3's drop-don't-block job).
///
///      External wrappers expose the internal calldata-slice decoders so they can be
///      driven through `vm.expectRevert` (mirrors BodyVersion.t.sol).
contract BridgeMsgCodecValidationTest is Test {
    // --- fixed-width decoders reject over-long payloads ---

    function test_AuctionStageStart_OverLong_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageStart(1, 100, 200, 300, 1e18, 1e6, 2e6, 3e6, 1);
        bytes memory tooLong = abi.encodePacked(packet, hex"00"); // 61 bytes, expected 60
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_STAGE_START,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_START
            )
        );
        this.exposedDecodeAuctionStageStart(tooLong);
    }

    // --- fixed-width decoders reject empty / truncated payloads with a typed error ---

    function test_AuctionStageStart_Empty_RevertsTyped() public {
        bytes memory empty = "";
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_STAGE_START,
                0,
                BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_START
            )
        );
        this.exposedDecodeAuctionStageStart(empty);
    }

    function test_AuctionStageStart_Truncated_RevertsTyped() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageStart(1, 100, 200, 300, 1e18, 1e6, 2e6, 3e6, 1);
        bytes memory truncated = new bytes(packet.length - 1); // 59 bytes
        for (uint256 i = 0; i < truncated.length; i++) {
            truncated[i] = packet[i];
        }
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_STAGE_START,
                truncated.length,
                BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_START
            )
        );
        this.exposedDecodeAuctionStageStart(truncated);
    }

    // --- remaining fixed-width decoders reject over-long payloads ---

    function test_AuctionStageReveal_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeAuctionStageReveal(1, true), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_STAGE_REVEAL,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_REVEAL
            )
        );
        this.exposedDecodeAuctionStageReveal(tooLong);
    }

    function test_AuctionStageClearing_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeAuctionStageClearing(1), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_CLEARING
            )
        );
        this.exposedDecodeAuctionStageClearing(tooLong);
    }

    function test_AuctionResult_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeAuctionResult(1, 7, 5e6, 3), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_AUCTION_RESULT,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_AUCTION_RESULT
            )
        );
        this.exposedDecodeAuctionResult(tooLong);
    }

    function test_MarkCalled_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeMarkCalled(1), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_MARK_CALLED,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_MARK_CALLED
            )
        );
        this.exposedDecodeMarkCalled(tooLong);
    }

    function test_MarkQualified_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeMarkQualified(1), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_MARK_QUALIFIED,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_MARK_QUALIFIED
            )
        );
        this.exposedDecodeMarkQualified(tooLong);
    }

    // --- isGreenDay decodes strictly (only 0x00 / 0x01) ---

    function test_AuctionStageReveal_GreenDayByteTwo_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(1, false);
        packet[6] = 0x02; // corrupt the flag byte
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidGreenDayFlag.selector, uint8(2)));
        this.exposedDecodeAuctionStageReveal(packet);
    }

    function testFuzz_AuctionStageReveal_NonBooleanByte_Reverts(uint8 flag) public {
        flag = uint8(bound(flag, 2, 255));
        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(1, false);
        packet[6] = bytes1(flag);
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidGreenDayFlag.selector, flag));
        this.exposedDecodeAuctionStageReveal(packet);
    }

    function test_AuctionStageReveal_BooleanBytes_RoundTrip() public view {
        (uint32 sFalse, bool gFalse) =
            this.exposedDecodeAuctionStageReveal(BridgeMsgCodec.encodeAuctionStageReveal(3, false));
        assertEq(sFalse, 3);
        assertFalse(gFalse);
        (uint32 sTrue, bool gTrue) =
            this.exposedDecodeAuctionStageReveal(BridgeMsgCodec.encodeAuctionStageReveal(4, true));
        assertEq(sTrue, 4);
        assertTrue(gTrue);
    }

    // --- Happy-path round-trips still pass after the exact-length guard ---

    function test_FixedWidth_RoundTrips_StillPass() public view {
        (uint32 s,,,,,,,,) =
            this.exposedDecodeAuctionStageStart(BridgeMsgCodec.encodeAuctionStageStart(42, 1, 2, 3, 1e18, 1, 2, 3, 1));
        assertEq(s, 42, "stageStart");
        assertEq(this.exposedDecodeAuctionStageClearing(BridgeMsgCodec.encodeAuctionStageClearing(7)), 7, "clearing");
        (uint32 rs,,,) = this.exposedDecodeAuctionResult(BridgeMsgCodec.encodeAuctionResult(9, 1, 1, 1));
        assertEq(rs, 9, "result");
        assertEq(this.exposedDecodeMarkCalled(BridgeMsgCodec.encodeMarkCalled(11)), 11, "markCalled");
        assertEq(this.exposedDecodeMarkQualified(BridgeMsgCodec.encodeMarkQualified(12)), 12, "markQualified");
    }

    // --- outbound encoders cap payload arrays at MAX_PAYLOAD_ARRAY_LEN ---

    function test_EncodeBidsBatch_AtCap_Encodes() public pure {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN; // 64
        bytes memory encoded = BridgeMsgCodec.encodeBidsBatch(
            1, 30101, true, 1, new address[](n), new uint16[](n), new uint64[](n), new uint32[](n)
        );
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_BIDS_BATCH);
    }

    function test_EncodeBidsBatch_OverCap_Reverts() public {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN + 1; // 65
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.PayloadArrayTooLong.selector, uint256(n), BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN
            )
        );
        this.exposedEncodeBidsBatch(n);
    }

    function test_EncodeRefund_OverCap_Reverts() public {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN + 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.PayloadArrayTooLong.selector, uint256(n), BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN
            )
        );
        this.exposedEncodeRefund(n);
    }

    function test_EncodeIssuance_OverCap_Reverts() public {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN + 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.PayloadArrayTooLong.selector, uint256(n), BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN
            )
        );
        this.exposedEncodeIssuance(n);
    }

    /// @dev decodeRefundInstructions enforces a symmetric inbound cap so a peer compromise or a
    ///      future encoder change cannot deliver an oversized REFUND that exhausts the receiver's
    ///      gas in the per-bidder loop. Built by hand to bypass the now-capping encoder.
    function test_DecodeRefund_OverOutboundCap_RevertsRefundBatchTooLarge() public {
        uint256 n = uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN) + 1; // 65, over the outbound cap
        bytes memory overCap = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS,
            abi.encode(uint32(1), new address[](n), new uint64[](n), new uint64[](n))
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.RefundBatchTooLarge.selector, n, uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN)
            )
        );
        this.exposedDecodeRefundInstructions(overCap);
    }

    // --- Real payload-length ceiling vs the LayerZero message-size cap ---

    /// @notice The send-side Executor `maxMessageSize` configured for these pathways
    ///         (`scripts/shared/layerzero.ts` → `LZ_INFRA.maxMessageSize`). A send whose
    ///         encoded message exceeds this reverts on the source chain. This is the *byte*
    ///         ceiling only; destination gas (the per-item credit loop) is a separate and,
    ///         for the heavy paths, tighter limit — not measured here.
    uint256 internal constant LZ_MAX_MESSAGE_BYTES = 10_000;

    /// @dev Derives the largest array length whose encoded message still fits under
    ///      `LZ_MAX_MESSAGE_BYTES`, by measuring the actual per-item byte cost, and asserts
    ///      `MAX_PAYLOAD_ARRAY_LEN` sits under it. Regression guard: if a payload grows
    ///      (e.g. a new array/field), the real ceiling drops and this fails if the cap loses
    ///      headroom. `len0/len1/len2` are encoded lengths at 0/1/2 items.
    function _deriveCeilingAndAssertHeadroom(string memory label, uint256 len0, uint256 len1, uint256 len2) internal {
        uint256 perItem = len1 - len0;
        assertEq(len2 - len1, perItem, string.concat(label, ": per-item byte cost is not linear"));
        uint256 derivedMaxItems = (LZ_MAX_MESSAGE_BYTES - len0) / perItem;
        emit log_named_uint(string.concat(label, " bytes/item"), perItem);
        emit log_named_uint(string.concat(label, " real max items @ 10000B"), derivedMaxItems);
        assertGe(
            derivedMaxItems,
            BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN,
            string.concat(label, ": MAX_PAYLOAD_ARRAY_LEN exceeds the real byte ceiling")
        );
    }

    /// @notice Computes the real per-message array ceiling under the LZ byte cap and proves the
    ///         single system-wide `MAX_PAYLOAD_ARRAY_LEN = 64` clears every one of them. Run with
    ///         `-vv` to see the derived numbers (bids is the tightest at ~128 B/item).
    function test_RealPayloadByteCeiling_ClearsTheCap() public {
        _deriveCeilingAndAssertHeadroom(
            "bids",
            this.exposedEncodeBidsBatch(0).length,
            this.exposedEncodeBidsBatch(1).length,
            this.exposedEncodeBidsBatch(2).length
        );
        _deriveCeilingAndAssertHeadroom(
            "refund",
            this.exposedEncodeRefund(0).length,
            this.exposedEncodeRefund(1).length,
            this.exposedEncodeRefund(2).length
        );
        _deriveCeilingAndAssertHeadroom(
            "issuance",
            this.exposedEncodeIssuance(0).length,
            this.exposedEncodeIssuance(1).length,
            this.exposedEncodeIssuance(2).length
        );
    }

    // --- External wrappers ---

    function exposedDecodeAuctionStageStart(bytes calldata p)
        external
        pure
        returns (uint32, uint32, uint32, uint32, uint128, uint64, uint64, uint64, uint16)
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

    function exposedDecodeMarkQualified(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeMarkQualified(p);
    }

    function exposedDecodeRefundInstructions(bytes calldata p)
        external
        pure
        returns (uint32, address[] memory, uint64[] memory, uint64[] memory)
    {
        return BridgeMsgCodec.decodeRefundInstructions(p);
    }

    function exposedEncodeBidsBatch(uint16 n) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeBidsBatch(
            1, 30101, true, 1, new address[](n), new uint16[](n), new uint64[](n), new uint32[](n)
        );
    }

    function exposedEncodeRefund(uint16 n) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeRefundInstructions(1, new address[](n), new uint64[](n), new uint64[](n));
    }

    function exposedEncodeIssuance(uint16 n) external pure returns (bytes memory) {
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = 1;
        payload.recipients = new address[](n);
        payload.quantities = new uint256[](n);
        return BridgeMsgCodec.encodeIssuanceInstructions(payload);
    }
}
