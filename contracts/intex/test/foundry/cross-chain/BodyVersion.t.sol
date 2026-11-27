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

/// @dev Exercises the body-version contract on both codecs:
///      - every encoder emits `bodyVersion == BODY_VERSION_V1` at offset 0.
///      - every decoder reverts `UnsupportedBodyVersion(got)` on any other leading byte.
///      - round-trip preserves the version byte alongside the payload.
contract BodyVersionTest is Test {
    /// @dev Fixed call stamp. These tests exercise the wire, not the clock.
    uint32 internal constant CALLED_AT = 1_777_000_000;

    // --- BridgeMsgCodec: encoder emits version byte ---

    function test_BridgeCodec_AllEncodersEmitVersionV1() public pure {
        bytes memory encoded;

        encoded = BridgeMsgCodec.encodeBidsPage(1, 30101, 0, 1, new address[](0), new uint256[](0));
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "bidsBatch.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_BIDS_PAGE, "bidsBatch.msgType");

        encoded = BridgeMsgCodec.encodeIntexAuctionStart(
            1, 100, 200, 300, 1e6, 1e6, ReferenceCurrencyPriceLib.one(840, 2e6, 4e6), 5, 6, 7, 1, 9e6
        );
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "stageStart.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_AUCTION_START, "stageStart.msgType");
        assertEq(
            encoded.length,
            BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_START + BridgeMsgCodec.REFERENCE_PRICE_LEN,
            "stageStart.length"
        );

        encoded = BridgeMsgCodec.encodeIntexAuctionClearingStart(1);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "stageClearing.version");

        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START, "stageClearing.msgType");
        assertEq(encoded.length, 6, "stageClearing.length");

        encoded = BridgeMsgCodec.encodeBidsPause(1, 2, 3, 4);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "bidsRemaining.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_BIDS_PAUSE, "bidsRemaining.msgType");
        assertEq(encoded.length, BridgeMsgCodec.MIN_LEN_BIDS_PAUSE, "bidsRemaining.length");

        encoded = BridgeMsgCodec.encodeIntexAuctionResult(1, AuctionResultLib.day(7, 5e6, 3));
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "auctionResult.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_AUCTION_RESULT, "auctionResult.msgType");
        assertEq(encoded.length, BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_RESULT, "auctionResult.length");

        encoded =
            BridgeMsgCodec.encodeIntexSeriesCallNotice(20260212, CALLED_AT, CallNoticeBatchLib.one("20260212-TRY-U"));
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "callSeries.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE, "callSeries.msgType");
        assertEq(encoded.length, BridgeMsgCodec.MIN_LEN_INTEX_SERIES_CALL_NOTICE, "callSeries.length");

        encoded = BridgeMsgCodec.encodeIntexAuctionWinners(1, 0, 1, 0, 0, new address[](0), 0, 0);
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "refund.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS, "refund.msgType");

        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = "20260212-TRY-U";
        encoded = BridgeMsgCodec.encodeIntexSeries(0, 0, 1, IssuanceBatchLib.one(payload));
        assertEq(uint8(encoded[0]), BridgeMsgCodec.BODY_VERSION_V1, "issuance.version");
        assertEq(uint8(encoded[1]), BridgeMsgCodec.MSG_INTEX_SERIES, "issuance.msgType");
    }

    // --- BridgeMsgCodec: round-trip ---

    function test_BridgeCodec_AuctionStageStart_RoundTrip() public view {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionStart(
            42, 100, 200, 300, 1e6, 5e6, ReferenceCurrencyPriceLib.one(10, 7e6, 13e6), 5, 6, 7, 3, 17e6
        );
        (uint32 emissionDay, IIntexAuction.AuctionSchedule memory schedule, IIntexAuction.AuctionParams memory params) =
            BridgeMsgCodec.decodeAuctionParams(packet);

        assertEq(emissionDay, 42);
        assertEq(schedule.commitEnd, 100);
        assertEq(schedule.revealEnd, 200);
        assertEq(schedule.issuanceEnd, 300);
        assertEq(params.prices[0].isoCode, 10);
        assertEq(params.promisLoadMinor, 1e6);
        assertEq(params.minBidRate, 5e6);
        assertEq(params.prices[0].entryPriceMinor, 7e6);
        assertEq(params.prices[0].callPriceMinor, 13e6);
        assertEq(params.callTrigger.callNoticePeriod, 5);
        assertEq(params.callTrigger.callWindow, 6);
        assertEq(params.callTrigger.callThreshold, 7);
        assertEq(params.minBidQuantity, 3);
        assertEq(params.bidBondMinor, 17e6);
    }

    function test_BridgeCodec_AuctionResult_RoundTrip() public view {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionResult(42, AuctionResultLib.day(7, 13e6, 5));
        (uint32 emissionDay, BridgeMsgCodec.IntexAuctionChainResult memory r) = this.exposedDecodeAuctionResult(packet);
        assertEq(emissionDay, 42);
        assertEq(r.quantity, 7);
        assertEq(r.clearingRate, 13e6);
        assertEq(r.wonBidsCount, 5);
    }

    function test_BridgeCodec_CallNotice_RoundTrip() public view {
        bytes memory packet =
            BridgeMsgCodec.encodeIntexSeriesCallNotice(20260212, CALLED_AT, CallNoticeBatchLib.one("20260212-TRY-U"));
        bytes14 seriesId = this.exposedDecodeIntexSeriesCallNotice(packet);
        assertEq(seriesId, bytes14("20260212-TRY-U"));
    }

    // --- BridgeMsgCodec: unknown version reverts ---

    function test_BridgeCodec_UnknownBodyVersion_AuctionStageStart_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionStart(
            1, 100, 200, 300, 1e6, 1e6, ReferenceCurrencyPriceLib.one(840, 2e6, 4e6), 5, 6, 7, 1, 9e6
        );
        packet[0] = 0xFF;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0xFF));
        BridgeMsgCodec.decodeAuctionParams(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_AuctionResult_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionResult(1, AuctionResultLib.day(1, 1, 1));
        packet[0] = 0x42;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x42));
        this.exposedDecodeAuctionResult(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_CallNotice_Reverts() public {
        bytes memory packet =
            BridgeMsgCodec.encodeIntexSeriesCallNotice(20260212, CALLED_AT, CallNoticeBatchLib.one("20260212-TRY-U"));
        packet[0] = 0xAA;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0xAA));
        this.exposedDecodeIntexSeriesCallNotice(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_BidsBatch_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeBidsPage(1, 30101, 0, 1, new address[](0), new uint256[](0));
        packet[0] = 0x99;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x99));
        this.exposedDecodeBidsPage(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_AuctionStageCollectingBids_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionClearingStart(1);
        packet[0] = 0x10;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x10));
        this.exposedDecodeAuctionStageCollectingBids(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_RefundInstructions_Reverts() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionWinners(1, 0, 1, 0, 0, new address[](0), 0, 0);
        packet[0] = 0x55;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x55));
        this.exposedDecodeRefundInstructions(packet);
    }

    function test_BridgeCodec_UnknownBodyVersion_IssuanceInstructions_Reverts() public {
        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = "20260212-TRY-U";
        bytes memory packet = BridgeMsgCodec.encodeIntexSeries(0, 0, 1, IssuanceBatchLib.one(payload));
        packet[0] = 0x77;
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, 0x77));
        this.exposedDecodeIssuanceInstructions(packet);
    }

    // --- BridgeMsgCodec: sibling-array parity + cap on the variable-length decoders ---

    function test_BridgeCodec_DecodeBidsBatch_RejectsArrayLengthMismatch() public {
        // Four parallel arrays with mismatched lengths. They are indexed in lockstep downstream, so
        // an unequal decode would panic out of bounds inside the ordered lane. Must revert typed.
        // The encoder now reverts on parity mismatch, so the test hand-builds the wire payload
        // directly. This matches the way an oversized REFUND would arrive via a peer compromise or
        // a future encoder change.
        address[] memory bidders = new address[](2);
        uint16[] memory quantities = new uint16[](1); // short
        uint32[] memory rates = new uint32[](2);
        uint32[] memory timestamps = new uint32[](2);
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_BIDS_PAGE,
            abi.encode(uint32(42), uint32(30101), uint16(0), uint16(1), bidders, quantities, rates, timestamps)
        );

        vm.expectRevert(
            abi.encodeWithSelector(BridgeMsgCodec.IntexAuctionBidsArrayLengthMismatch.selector, uint256(2), uint256(1))
        );
        this.exposedDecodeBidsPage(packet);
    }

    function test_BridgeCodec_BidsBatch_BatchIndexTotal_RoundTrips() public view {
        address[] memory bidders = new address[](1);
        bidders[0] = address(0xB1);
        uint16[] memory quantities = new uint16[](1);
        quantities[0] = 7;
        uint32[] memory rates = new uint32[](1);
        rates[0] = 100;
        uint32[] memory timestamps = new uint32[](1);
        timestamps[0] = 42;

        bytes memory lastPacket =
            BridgeMsgCodec.encodeBidsPage(1, 30101, 1, 2, bidders, BidPackLib.pack(quantities, rates, timestamps));
        (,, uint16 idxLast, uint16 totalLast,,) = this.exposedDecodeBidsPage(lastPacket);
        assertEq(idxLast, 1, "pageIndex should round-trip");
        assertEq(totalLast, 2, "totalPages should round-trip");

        bytes memory midPacket =
            BridgeMsgCodec.encodeBidsPage(1, 30101, 0, 2, bidders, BidPackLib.pack(quantities, rates, timestamps));
        (,, uint16 idxMid, uint16 totalMid,,) = this.exposedDecodeBidsPage(midPacket);
        assertEq(idxMid, 0, "pageIndex=0 should round-trip");
        assertEq(totalMid, 2, "totalPages should round-trip");
    }

    // --- External wrappers so calldata-slice helpers can be invoked via vm.expectRevert ---

    function exposedDecodeBidsPage(bytes calldata p)
        external
        pure
        returns (uint32, uint32, uint16, uint16, address[] memory, uint256[] memory)
    {
        return BridgeMsgCodec.decodeBidsPage(p);
    }

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

    function exposedDecodeIssuanceInstructions(bytes calldata p)
        external
        pure
        returns (BridgeMsgCodec.IntexSeriesPayload[] memory series)
    {
        (,,, series) = BridgeMsgCodec.decodeIntexSeries(p);
    }
}
