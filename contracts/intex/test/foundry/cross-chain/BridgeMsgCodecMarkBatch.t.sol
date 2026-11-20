// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";

/// @dev MARK_CALLED carries one day's series in one reference currency, so it round-trips a batch and
///      refuses an empty or over-sized one.
contract BridgeMsgCodecMarkBatchTest is Test {
    /// @dev Fixed call stamp. These tests exercise the wire, not the clock.
    uint32 internal constant CALLED_AT = 1_777_000_000;

    uint32 internal constant EMISSION_DAY = 20260212;
    bytes14 internal constant FIRST = "20260212-USD-U";
    bytes14 internal constant SECOND = "20260212-EUR-U";

    function exposedDecodeMarkCalled(bytes calldata p)
        external
        pure
        returns (uint32 emissionDay, uint64 calledAt, bytes14[] memory seriesIds)
    {
        return BridgeMsgCodec.decodeIntexSeriesCallNotice(p);
    }

    function exposedEncodeMarkCalled(uint32 day, bytes14[] calldata ids) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexSeriesCallNotice(day, CALLED_AT, ids);
    }

    function test_markCalledRoundTripsTheWholeBatch() public view {
        bytes memory packet =
            BridgeMsgCodec.encodeIntexSeriesCallNotice(EMISSION_DAY, CALLED_AT, MarkBatchLib.two(FIRST, SECOND));

        (uint32 day,, bytes14[] memory ids) = this.exposedDecodeMarkCalled(packet);

        assertEq(day, EMISSION_DAY, "emissionDay survives");
        assertEq(ids.length, 2, "both series survive");
        assertEq(ids[0], FIRST, "first series");
        assertEq(ids[1], SECOND, "second series");
    }

    function test_aFullBatchRoundTrips() public view {
        uint256 max = BridgeMsgCodec.MAX_SERIES_PER_CALL_NOTICE;
        bytes14[] memory batch = MarkBatchLib.sized(FIRST, max);

        (,, bytes14[] memory ids) =
            this.exposedDecodeMarkCalled(BridgeMsgCodec.encodeIntexSeriesCallNotice(EMISSION_DAY, CALLED_AT, batch));

        assertEq(ids.length, max, "the cap round-trips");
        for (uint256 i = 0; i < max; ++i) {
            // Distinct ids, so this pins the order and not just the count.
            assertEq(ids[i], batch[i], "series survives in place");
            if (i > 0) assertTrue(batch[i] != batch[i - 1], "the fixture ids differ");
        }
    }

    function test_theHeaderNamesTheMessageType() public view {
        bytes memory called =
            BridgeMsgCodec.encodeIntexSeriesCallNotice(EMISSION_DAY, CALLED_AT, MarkBatchLib.one(FIRST));

        assertEq(uint8(called[0]), BridgeMsgCodec.BODY_VERSION_V1, "called version");
        assertEq(uint8(called[1]), BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE, "called type");
    }

    function test_anEmptyBatchIsRefusedOnBothSides() public {
        bytes14[] memory empty = new bytes14[](0);

        vm.expectRevert(BridgeMsgCodec.EmptyCallNoticePage.selector);
        this.exposedEncodeMarkCalled(EMISSION_DAY, empty);

        // Inbound too, though the length floor gets there first: an empty batch encodes
        // shorter than the one-series minimum, so it never reaches the batch check.
        bytes memory forged = abi.encodePacked(hex"0108", abi.encode(EMISSION_DAY, empty));
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE,
                forged.length,
                BridgeMsgCodec.MIN_LEN_INTEX_SERIES_CALL_NOTICE
            )
        );
        this.exposedDecodeMarkCalled(forged);
    }

    function test_anOverSizedBatchIsRefusedOnBothSides() public {
        uint256 tooMany = BridgeMsgCodec.MAX_SERIES_PER_CALL_NOTICE + 1;
        bytes14[] memory batch = MarkBatchLib.sized(FIRST, tooMany);
        bytes memory expected = abi.encodeWithSelector(
            BridgeMsgCodec.CallNoticePageTooLarge.selector, tooMany, BridgeMsgCodec.MAX_SERIES_PER_CALL_NOTICE
        );

        vm.expectRevert(expected);
        this.exposedEncodeMarkCalled(EMISSION_DAY, batch);

        bytes memory forged = abi.encodePacked(hex"0108", abi.encode(EMISSION_DAY, CALLED_AT, batch));
        vm.expectRevert(expected);
        this.exposedDecodeMarkCalled(forged);
    }

    function test_aBodyShorterThanOneSeriesIsRefused() public {
        bytes memory tooShort = abi.encodePacked(hex"0108", new bytes(128));
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE,
                tooShort.length,
                BridgeMsgCodec.MIN_LEN_INTEX_SERIES_CALL_NOTICE
            )
        );
        this.exposedDecodeMarkCalled(tooShort);
    }
}
