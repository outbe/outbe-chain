// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Golden-value and per-field round-trip coverage for BridgeMsgCodec encode/decode.
contract BridgeMsgCodecGoldenTest is Test {
    // Byte-literal goldens for the fixed-width packed messages.

    function test_Golden_AuctionStageStart() public pure {
        bytes memory encoded = BridgeMsgCodec.encodeAuctionStageStart(
            0x11223344,
            0x55667788,
            0x99AABBCC,
            0xDDEEFF00,
            0x0102030405060708090A0B0C0D0E0F10,
            0x1A2B3C4D,
            0x1122334455667788,
            0x99AABBCCDDEEFF00,
            0xA1B2C3D4E5F60718,
            0xCAFEBABE,
            0x5678,
            0x9ABC,
            0xABCD
        );
        assertEq(
            encoded,
            hex"0104112233445566778899aabbccddeeff000102030405060708090a0b0c0d0e0f101a2b3c4d112233445566778899aabbccddeeff00a1b2c3d4e5f60718cafebabe56789abcabcd"
        );
        assertEq(encoded.length, BridgeMsgCodec.MIN_LEN_AUCTION_STAGE_START);
    }

    function test_Golden_AuctionStageReveal() public pure {
        assertEq(BridgeMsgCodec.encodeAuctionStageReveal(0x0A0B0C0D, true), hex"01050a0b0c0d01");
        assertEq(BridgeMsgCodec.encodeAuctionStageReveal(0x0A0B0C0D, false), hex"01050a0b0c0d00");
    }

    function test_Golden_AuctionStageClearing() public pure {
        assertEq(BridgeMsgCodec.encodeAuctionStageClearing(0x0A0B0C0D), hex"01060a0b0c0d");
    }

    function test_Golden_AuctionResult() public pure {
        bytes memory encoded =
            BridgeMsgCodec.encodeAuctionResult(0x11223344, 0x55667788, 0x99AABBCCDDEEFF00, 0xA1B2C3D4);
        assertEq(encoded, hex"0107112233445566778899aabbccddeeff00a1b2c3d4");
        assertEq(encoded.length, BridgeMsgCodec.MIN_LEN_AUCTION_RESULT);
    }

    function test_Golden_MarkCalled() public pure {
        assertEq(BridgeMsgCodec.encodeMarkCalled(0x11223344), hex"010a11223344");
    }

    function test_Golden_MarkQualified() public pure {
        assertEq(BridgeMsgCodec.encodeMarkQualified(0x11223344), hex"010b11223344");
    }

    // Per-field round-trips: a distinct sentinel per field, so any offset or tuple
    // reorder lands a value in the wrong field and fails an assertion.

    function test_RoundTrip_AuctionStageStart_AllFields() public view {
        (
            uint32 seriesId,
            uint32 commitEnd,
            uint32 revealEnd,
            uint32 issuanceEnd,
            uint128 promisLoadMinor,
            uint32 minIntexBidRate,
            uint64 entryPrice,
            uint64 floorPriceMinor,
            uint64 callPriceMinor,
            uint32 intexCallPeriod,
            uint16 callWindowDays,
            uint16 callThresholdDays,
            uint16 minIntexBidQuantity
        ) = this.exposedDecodeAuctionStageStart(
            BridgeMsgCodec.encodeAuctionStageStart(
                0x11223344,
                0x55667788,
                0x99AABBCC,
                0xDDEEFF00,
                0x0102030405060708090A0B0C0D0E0F10,
                0x1A2B3C4D,
                0x1122334455667788,
                0x99AABBCCDDEEFF00,
                0xA1B2C3D4E5F60718,
                0xCAFEBABE,
                0x5678,
                0x9ABC,
                0xABCD
            )
        );
        assertEq(seriesId, 0x11223344, "seriesId");
        assertEq(commitEnd, 0x55667788, "commitEnd");
        assertEq(revealEnd, 0x99AABBCC, "revealEnd");
        assertEq(issuanceEnd, 0xDDEEFF00, "issuanceEnd");
        assertEq(promisLoadMinor, 0x0102030405060708090A0B0C0D0E0F10, "promisLoadMinor");
        assertEq(minIntexBidRate, 0x1A2B3C4D, "minIntexBidRate");
        assertEq(entryPrice, 0x1122334455667788, "entryPrice");
        assertEq(floorPriceMinor, 0x99AABBCCDDEEFF00, "floorPriceMinor");
        assertEq(callPriceMinor, 0xA1B2C3D4E5F60718, "callPriceMinor");
        assertEq(intexCallPeriod, 0xCAFEBABE, "intexCallPeriod");
        assertEq(callWindowDays, 0x5678, "callWindowDays");
        assertEq(callThresholdDays, 0x9ABC, "callThresholdDays");
        assertEq(minIntexBidQuantity, 0xABCD, "minIntexBidQuantity");
    }

    function test_RoundTrip_AuctionResult_AllFields() public view {
        (uint32 seriesId, uint32 issuedIntexCount, uint64 clearingPrice, uint32 wonBidsCount) = this.exposedDecodeAuctionResult(
            BridgeMsgCodec.encodeAuctionResult(0x11223344, 0x55667788, 0x99AABBCCDDEEFF00, 0xA1B2C3D4)
        );
        assertEq(seriesId, 0x11223344, "seriesId");
        assertEq(issuedIntexCount, 0x55667788, "issuedIntexCount");
        assertEq(clearingPrice, 0x99AABBCCDDEEFF00, "clearingPrice");
        assertEq(wonBidsCount, 0xA1B2C3D4, "wonBidsCount");
    }

    function test_RoundTrip_BidsBatch_AllFields_InclRelayGeneration() public view {
        address[] memory bidders = new address[](2);
        bidders[0] = address(0xA11CE);
        bidders[1] = address(0xB0B);
        uint16[] memory quantities = new uint16[](2);
        quantities[0] = 0x1111;
        quantities[1] = 0x2222;
        uint32[] memory rates = new uint32[](2);
        rates[0] = 0x33333333;
        rates[1] = 0x44444444;
        uint32[] memory timestamps = new uint32[](2);
        timestamps[0] = 0x55555555;
        timestamps[1] = 0x66666666;

        (
            uint32 seriesId,
            uint32 srcEid,
            bool isLast,
            uint32 relayGeneration,
            address[] memory dBidders,
            uint16[] memory dQuantities,
            uint32[] memory dRates,
            uint32[] memory dTimestamps
        ) = this.exposedDecodeBidsBatch(
            BridgeMsgCodec.encodeBidsBatch(
                0x11223344, 0x0000ABCD, true, 0x0000002A, bidders, quantities, rates, timestamps
            )
        );

        assertEq(seriesId, 0x11223344, "seriesId");
        assertEq(srcEid, 0x0000ABCD, "srcEid");
        assertTrue(isLast, "isLast");
        assertEq(relayGeneration, 0x0000002A, "relayGeneration");
        assertEq(dBidders.length, 2, "bidders len");
        assertEq(dBidders[0], address(0xA11CE), "bidders[0]");
        assertEq(dBidders[1], address(0xB0B), "bidders[1]");
        assertEq(dQuantities[0], 0x1111, "quantities[0]");
        assertEq(dQuantities[1], 0x2222, "quantities[1]");
        assertEq(dRates[0], 0x33333333, "rates[0]");
        assertEq(dRates[1], 0x44444444, "rates[1]");
        assertEq(dTimestamps[0], 0x55555555, "timestamps[0]");
        assertEq(dTimestamps[1], 0x66666666, "timestamps[1]");
    }

    function test_RoundTrip_BidsBatch_IsLastFalse_RelayGenerationOne() public view {
        (,, bool isLast, uint32 relayGeneration,,,,) = this.exposedDecodeBidsBatch(
            BridgeMsgCodec.encodeBidsBatch(
                7, 30101, false, 1, new address[](0), new uint16[](0), new uint32[](0), new uint32[](0)
            )
        );
        assertFalse(isLast, "isLast");
        assertEq(relayGeneration, 1, "relayGeneration");
    }

    function test_RoundTrip_RefundInstructions_AllFields() public view {
        address[] memory bidders = new address[](2);
        bidders[0] = address(0xA11CE);
        bidders[1] = address(0xB0B);
        uint64[] memory refunded = new uint64[](2);
        refunded[0] = 0x1111111111111111;
        refunded[1] = 0x2222222222222222;
        uint64[] memory paid = new uint64[](2);
        paid[0] = 0x3333333333333333;
        paid[1] = 0x4444444444444444;

        (uint32 seriesId, address[] memory dBidders, uint64[] memory dRefunded, uint64[] memory dPaid) = this.exposedDecodeRefundInstructions(
            BridgeMsgCodec.encodeRefundInstructions(0x77665544, bidders, refunded, paid)
        );

        assertEq(seriesId, 0x77665544, "seriesId");
        assertEq(dBidders[0], address(0xA11CE), "bidders[0]");
        assertEq(dBidders[1], address(0xB0B), "bidders[1]");
        assertEq(dRefunded[0], 0x1111111111111111, "refunded[0]");
        assertEq(dRefunded[1], 0x2222222222222222, "refunded[1]");
        assertEq(dPaid[0], 0x3333333333333333, "paid[0]");
        assertEq(dPaid[1], 0x4444444444444444, "paid[1]");
    }

    function test_RoundTrip_IssuanceInstructions_AllFields() public view {
        address[] memory recipients = new address[](2);
        recipients[0] = address(0xA11CE);
        recipients[1] = address(0xB0B);
        uint256[] memory quantities = new uint256[](2);
        quantities[0] = 0xDEAD;
        quantities[1] = 0xBEEF;

        BridgeMsgCodec.IssuanceInstructionsPayload memory p;
        p.seriesId = 0x11223344;
        p.issuedIntexCount = 0x55667788;
        p.promisLoadMinor = 0x0102030405060708090A0B0C0D0E0F10;
        p.costAmountMinor = 0x1122334455667788;
        p.floorPriceMinor = 0x99AABBCCDDEEFF00;
        p.intexCallPeriod = 0xCAFEBABE;
        p.referenceCurrency = 0x1234;
        p.callWindowDays = 0x5678;
        p.callThresholdDays = 0x9ABC;
        p.callPriceMinor = 0xA1B2C3D4E5F60718;
        p.recipients = recipients;
        p.quantities = quantities;

        BridgeMsgCodec.IssuanceInstructionsPayload memory d =
            this.exposedDecodeIssuanceInstructions(BridgeMsgCodec.encodeIssuanceInstructions(p));

        assertEq(d.seriesId, 0x11223344, "seriesId");
        assertEq(d.issuedIntexCount, 0x55667788, "issuedIntexCount");
        assertEq(d.promisLoadMinor, 0x0102030405060708090A0B0C0D0E0F10, "promisLoadMinor");
        assertEq(d.costAmountMinor, 0x1122334455667788, "costAmountMinor");
        assertEq(d.floorPriceMinor, 0x99AABBCCDDEEFF00, "floorPriceMinor");
        assertEq(d.intexCallPeriod, 0xCAFEBABE, "intexCallPeriod");
        assertEq(d.referenceCurrency, 0x1234, "referenceCurrency");
        assertEq(d.callWindowDays, 0x5678, "callWindowDays");
        assertEq(d.callThresholdDays, 0x9ABC, "callThresholdDays");
        assertEq(d.callPriceMinor, 0xA1B2C3D4E5F60718, "callPriceMinor");
        assertEq(d.recipients[0], address(0xA11CE), "recipients[0]");
        assertEq(d.recipients[1], address(0xB0B), "recipients[1]");
        assertEq(d.quantities[0], 0xDEAD, "quantities[0]");
        assertEq(d.quantities[1], 0xBEEF, "quantities[1]");
    }

    function test_RoundTrip_SingleField_SeriesId() public view {
        assertEq(
            this.exposedDecodeAuctionStageClearing(BridgeMsgCodec.encodeAuctionStageClearing(0x0A0B0C0D)), 0x0A0B0C0D
        );
        assertEq(this.exposedDecodeMarkCalled(BridgeMsgCodec.encodeMarkCalled(0x0A0B0C0D)), 0x0A0B0C0D);
        assertEq(this.exposedDecodeMarkQualified(BridgeMsgCodec.encodeMarkQualified(0x0A0B0C0D)), 0x0A0B0C0D);
        (uint32 s, bool g) =
            this.exposedDecodeAuctionStageReveal(BridgeMsgCodec.encodeAuctionStageReveal(0x0A0B0C0D, true));
        assertEq(s, 0x0A0B0C0D);
        assertTrue(g);
    }

    // External calldata wrappers for the internal decoders.

    function exposedDecodeAuctionStageStart(bytes calldata p)
        external
        pure
        returns (uint32, uint32, uint32, uint32, uint128, uint32, uint64, uint64, uint64, uint32, uint16, uint16, uint16)
    {
        return BridgeMsgCodec.decodeAuctionStageStart(p);
    }

    function exposedDecodeAuctionResult(bytes calldata p) external pure returns (uint32, uint32, uint64, uint32) {
        return BridgeMsgCodec.decodeAuctionResult(p);
    }

    function exposedDecodeAuctionStageReveal(bytes calldata p) external pure returns (uint32, bool) {
        return BridgeMsgCodec.decodeAuctionStageReveal(p);
    }

    function exposedDecodeAuctionStageClearing(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeAuctionStageClearing(p);
    }

    function exposedDecodeMarkCalled(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeMarkCalled(p);
    }

    function exposedDecodeMarkQualified(bytes calldata p) external pure returns (uint32) {
        return BridgeMsgCodec.decodeMarkQualified(p);
    }

    function exposedDecodeBidsBatch(bytes calldata p)
        external
        pure
        returns (uint32, uint32, bool, uint32, address[] memory, uint16[] memory, uint32[] memory, uint32[] memory)
    {
        return BridgeMsgCodec.decodeBidsBatch(p);
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
}
