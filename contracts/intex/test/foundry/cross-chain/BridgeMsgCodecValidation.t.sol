// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AuctionResultLib} from "../helpers/AuctionResultLib.sol";

import {CallNoticeBatchLib} from "../helpers/CallNoticeBatchLib.sol";
import {BidPackLib} from "../helpers/BidPackLib.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";

/// @dev PR-A Tier-1 input-validation hardening of BridgeMsgCodec:
///      - fixed-width decoders assert exact length (truncation silent-truncation).
///      - outbound encoders cap payload arrays at `MAX_PAYLOAD_ARRAY_LEN`.
///      - `decode*` deliberately does NOT cap (inbound is A3's drop-don't-block job).
///
///      External wrappers expose the internal calldata-slice decoders so they can be
///      driven through `vm.expectRevert` (mirrors BodyVersion.t.sol).
contract BridgeMsgCodecValidationTest is Test {
    /// @dev Fixed call stamp. These tests exercise the wire, not the clock.
    uint32 internal constant CALLED_AT = 1_777_000_000;

    // --- fixed-width decoders reject over-long payloads ---

    function test_AuctionStageStart_OverLong_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionStart(
            1, 100, 200, 300, 1e6, 1e6, ReferenceCurrencyPriceLib.one(840, 2e6, 4e6), 5, 6, 7, 1, 9e6
        );
        bytes memory tooLong = abi.encodePacked(packet, hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_START,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_START
            )
        );
        BridgeMsgCodec.decodeAuctionParams(tooLong);
    }

    // --- fixed-width decoders reject empty / truncated payloads with a typed error ---

    function test_AuctionStageStart_Empty_RevertsTyped() public {
        bytes memory empty = "";
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_START,
                0,
                BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_START
            )
        );
        BridgeMsgCodec.decodeAuctionParams(empty);
    }

    function test_AuctionStageStart_Truncated_RevertsTyped() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionStart(
            1, 100, 200, 300, 1e6, 1e6, ReferenceCurrencyPriceLib.one(840, 2e6, 4e6), 5, 6, 7, 1, 9e6
        );
        bytes memory truncated = new bytes(packet.length - 1);
        for (uint256 i = 0; i < truncated.length; i++) {
            truncated[i] = packet[i];
        }
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_START,
                truncated.length,
                BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_START
            )
        );
        BridgeMsgCodec.decodeAuctionParams(truncated);
    }

    // --- remaining fixed-width decoders reject over-long payloads ---

    function test_AuctionStageCollectingBids_OverLong_Reverts() public {
        bytes memory tooLong = abi.encodePacked(BridgeMsgCodec.encodeIntexAuctionClearingStart(1), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_CLEARING_START
            )
        );
        this.exposedDecodeAuctionStageCollectingBids(tooLong);
    }

    function test_AuctionResult_OverLong_Reverts() public {
        bytes memory tooLong =
            abi.encodePacked(BridgeMsgCodec.encodeIntexAuctionResult(1, AuctionResultLib.day(7, 5e6, 3)), hex"00");
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_RESULT,
                tooLong.length,
                BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_RESULT
            )
        );
        this.exposedDecodeAuctionResult(tooLong);
    }

    function test_CallNotice_EmptyBatch_Reverts() public {
        bytes14[] memory empty = new bytes14[](0);
        vm.expectRevert(BridgeMsgCodec.EmptyCallNoticePage.selector);
        this.exposedEncodeIntexSeriesCallNotice(20260212, empty);
    }

    function test_CallNotice_OverSizedBatch_Reverts() public {
        uint256 tooMany = BridgeMsgCodec.MAX_SERIES_PER_CALL_NOTICE + 1;
        bytes14[] memory batch = CallNoticeBatchLib.sized("20260212-TRY-U", tooMany);
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.CallNoticePageTooLarge.selector, tooMany, BridgeMsgCodec.MAX_SERIES_PER_CALL_NOTICE
            )
        );
        this.exposedEncodeIntexSeriesCallNotice(20260212, batch);
    }

    function test_CallNotice_ShortBody_Reverts() public {
        bytes memory tooShort = abi.encodePacked(hex"0108", new bytes(64));
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE,
                tooShort.length,
                BridgeMsgCodec.MIN_LEN_INTEX_SERIES_CALL_NOTICE
            )
        );
        this.exposedDecodeIntexSeriesCallNotice(tooShort);
    }

    // --- Happy-path round-trips still pass after the exact-length guard ---

    function test_FixedWidth_RoundTrips_StillPass() public view {
        (uint32 s,,) = BridgeMsgCodec.decodeAuctionParams(
            BridgeMsgCodec.encodeIntexAuctionStart(
                42, 1, 2, 3, 1e6, 1, ReferenceCurrencyPriceLib.one(840, 2, 4e6), 5, 6, 7, 1, 9e6
            )
        );
        assertEq(s, 42, "stageStart");
        assertEq(
            this.exposedDecodeAuctionStageCollectingBids(BridgeMsgCodec.encodeIntexAuctionClearingStart(7)),
            7,
            "clearing"
        );
        (uint32 rs,) =
            this.exposedDecodeAuctionResult(BridgeMsgCodec.encodeIntexAuctionResult(9, AuctionResultLib.day(1, 1, 1)));
        assertEq(rs, 9, "result");
        assertEq(
            this.exposedDecodeIntexSeriesCallNotice(
                BridgeMsgCodec.encodeIntexSeriesCallNotice(
                    20260212, CALLED_AT, CallNoticeBatchLib.one("20260212-TRY-U")
                )
            ),
            bytes14("20260212-TRY-U"),
            "callSeries"
        );
    }

    // --- outbound encoders cap payload arrays at MAX_PAYLOAD_ARRAY_LEN ---

    function test_EncodeBidsBatch_AtCap_Encodes() public pure {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN; // 64
        bytes memory encoded = BridgeMsgCodec.encodeBidsPage(1, 30101, 0, 1, new address[](n), new uint256[](n));
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_BIDS_PAGE);
    }

    function test_EncodeBidsBatch_OverCap_Reverts() public {
        uint16 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN + 1;
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
        uint16 n = BridgeMsgCodec.MAX_SERIES_PER_ISSUANCE + 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.IntexSeriesPageTooLarge.selector,
                uint256(n),
                uint256(BridgeMsgCodec.MAX_SERIES_PER_ISSUANCE)
            )
        );
        this.exposedEncodeIssuance(n);
    }

    /// @dev decodeIntexAuctionWinners enforces a symmetric inbound cap. Thus a peer compromise or a
    ///      future encoder change cannot deliver an oversized REFUND that exhausts the receiver's
    ///      gas in the per-winner loop. The test builds the payload by hand to bypass the
    ///      now-capping encoder.
    function test_DecodeRefund_OverOutboundCap_RevertsRefundBatchTooLarge() public {
        uint256 n = uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN) + 1;
        bytes memory overCap = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS,
            abi.encode(uint32(1), uint16(0), uint16(1), uint64(1), uint128(1), new address[](n), uint16(0), uint16(0))
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.IntexAuctionWinnersPageTooLarge.selector,
                n,
                uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN)
            )
        );
        this.exposedDecodeRefundInstructions(overCap);
    }

    // --- Real payload-length ceiling vs the ERC-7786 message-size cap ---

    /// @notice The send-side `maxMessageSize` the bridge configures for these pathways. A send whose
    ///         encoded message exceeds this reverts on the source chain. This is the *byte*
    ///         ceiling only. Destination gas (the per-item crosschainMint loop) is a separate
    ///         limit, and for the heavy paths a tighter one. This file does not measure it.
    uint256 internal constant MAX_MESSAGE_BYTES = 10_000;

    /// @dev Derives the largest array length whose encoded message still fits under
    ///      `MAX_MESSAGE_BYTES`, by measuring the actual per-item byte cost, and asserts
    ///      `MAX_PAYLOAD_ARRAY_LEN` sits under it. Regression guard: if a payload grows
    ///      (e.g. a new array/field), the real ceiling drops and this fails if the cap loses
    ///      headroom. `len0/len1/len2` are encoded lengths at 0/1/2 items.
    function _deriveCeilingAndAssertHeadroom(string memory label, uint256 len0, uint256 len1, uint256 len2) internal {
        uint256 perItem = len1 - len0;
        assertEq(len2 - len1, perItem, string.concat(label, ": per-item byte cost is not linear"));
        uint256 derivedMaxItems = (MAX_MESSAGE_BYTES - len0) / perItem;
        emit log_named_uint(string.concat(label, " bytes/item"), perItem);
        emit log_named_uint(string.concat(label, " real max items @ 10000B"), derivedMaxItems);
        assertGe(
            derivedMaxItems,
            BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN,
            string.concat(label, ": MAX_PAYLOAD_ARRAY_LEN exceeds the real byte ceiling")
        );
    }

    /// @notice Computes the real per-message array ceiling under the bridge byte cap and proves the
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
        // A message carries at least one series, and its cap is the series cap.
        uint256 one = this.exposedEncodeIssuance(1).length;
        uint256 perSeries = this.exposedEncodeIssuance(2).length - one;
        assertEq(
            this.exposedEncodeIssuance(3).length - this.exposedEncodeIssuance(2).length,
            perSeries,
            "issuance: per-series byte cost is not linear"
        );
        uint256 maxSeries = 1 + (MAX_MESSAGE_BYTES - one) / perSeries;
        emit log_named_uint("issuance bytes/series", perSeries);
        emit log_named_uint("issuance real max series @ 10000B", maxSeries);
        assertGe(
            maxSeries,
            BridgeMsgCodec.MAX_SERIES_PER_ISSUANCE,
            "issuance: MAX_SERIES_PER_ISSUANCE exceeds the real byte ceiling"
        );
    }

    // --- External wrappers ---

    function exposedDecodeAuctionStageCollectingBids(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeIntexAuctionClearingStart(p);
    }

    function exposedDecodeAuctionResult(bytes calldata p)
        external
        pure
        returns (uint32, BridgeMsgCodec.IntexAuctionChainResult memory)
    {
        return BridgeMsgCodec.decodeIntexAuctionResult(p);
    }

    function exposedEncodeIntexSeriesCallNotice(uint32 day, bytes14[] calldata ids)
        external
        pure
        returns (bytes memory)
    {
        return BridgeMsgCodec.encodeIntexSeriesCallNotice(day, CALLED_AT, ids);
    }

    /// @dev The arrival set a receiver keeps for a day is one word wide, so a claimed count past
    ///      `MAX_PAGES` has no slot to land in. Refunds bound this on both sides of the wire; issuance
    ///      has to bound it the same way.
    function test_Issuance_TotalChunksBeyondTheCeiling_Reverts() public {
        uint16 tooMany = BridgeMsgCodec.MAX_PAGES + 1;

        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidIntexSeriesPage.selector, uint16(0), tooMany));
        this.exposedEncodeIssuanceChunk(0, tooMany);
    }

    /// @dev A peer that hand-rolls the body bypasses the encoder, so the decoder must reject it too.
    function test_Issuance_TotalChunksBeyondTheCeiling_RejectedOnDecode() public {
        uint16 tooMany = BridgeMsgCodec.MAX_PAGES + 1;
        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = "20260212-TRY-U";
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_INTEX_SERIES,
            abi.encode(uint32(0), uint16(0), tooMany, IssuanceBatchLib.one(payload))
        );

        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidIntexSeriesPage.selector, uint16(0), tooMany));
        this.exposedDecodeIssuance(packet);
    }

    function exposedDecodeIntexSeriesCallNotice(bytes calldata p) external pure returns (bytes14) {
        (,, bytes14[] memory seriesIds) = BridgeMsgCodec.decodeIntexSeriesCallNotice(p);
        return seriesIds[0];
    }

    function exposedDecodeRefundInstructions(bytes calldata p)
        external
        pure
        returns (uint32, uint16, uint16, uint64, uint128, address[] memory, uint16, uint16)
    {
        return BridgeMsgCodec.decodeIntexAuctionWinners(p);
    }

    function exposedEncodeBidsBatch(uint16 n) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeBidsPage(1, 30101, 0, 1, new address[](n), new uint256[](n));
    }

    function exposedEncodeRefund(uint16 n) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexAuctionWinners(1, 0, 1, 1, 1, new address[](n), 0, 0);
    }

    function exposedEncodeIssuanceChunk(uint16 pageIndex, uint16 totalPages) external pure returns (bytes memory) {
        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = "20260212-TRY-U";
        return BridgeMsgCodec.encodeIntexSeries(0, pageIndex, totalPages, IssuanceBatchLib.one(payload));
    }

    function exposedDecodeIssuance(bytes calldata p) external pure {
        BridgeMsgCodec.decodeIntexSeries(p);
    }

    function exposedEncodeIssuance(uint16 n) external pure returns (bytes memory) {
        BridgeMsgCodec.IntexSeriesPayload[] memory series = new BridgeMsgCodec.IntexSeriesPayload[](n);
        for (uint256 i = 0; i < n; i++) {
            series[i].seriesId = "20260212-TRY-U";
        }
        return BridgeMsgCodec.encodeIntexSeries(0, 0, 1, series);
    }
}
