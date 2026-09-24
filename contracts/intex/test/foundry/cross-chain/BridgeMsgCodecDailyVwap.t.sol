// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";

contract BridgeMsgCodecDailyVwapTest is Test {
    uint32 internal constant UTC_DAY = 20260315;

    function _rows(uint256 count) internal pure returns (IOriginRouter.DailyVwap[] memory rows) {
        rows = new IOriginRouter.DailyVwap[](count);
        for (uint256 i = 0; i < count; ++i) {
            rows[i] = IOriginRouter.DailyVwap({isoCode: uint16(840 + i), vwapMinor: uint64(1_000_000 + i)});
        }
    }

    /// External so the decoder reads calldata, as the router does.
    function decode(bytes calldata message) external pure returns (uint32, IOriginRouter.DailyVwap[] memory) {
        return BridgeMsgCodec.decodeDailyVwap(message);
    }

    function test_TheLayoutIsPackedHeadAndRows() public pure {
        IOriginRouter.DailyVwap[] memory rows = new IOriginRouter.DailyVwap[](2);
        rows[0] = IOriginRouter.DailyVwap({isoCode: 840, vwapMinor: 1_500_000});
        rows[1] = IOriginRouter.DailyVwap({isoCode: 978, vwapMinor: 1_400_000});
        assertEq(
            BridgeMsgCodec.encodeDailyVwap(UTC_DAY, rows),
            abi.encodePacked(
                uint8(1),
                BridgeMsgCodec.MSG_DAILY_VWAP,
                UTC_DAY,
                uint8(2),
                uint16(840),
                uint64(1_500_000),
                uint16(978),
                uint64(1_400_000)
            )
        );
    }

    function test_ADayRoundTrips() public view {
        IOriginRouter.DailyVwap[] memory rows = _rows(BridgeMsgCodec.MAX_REFERENCE_PRICES);
        (uint32 utcDay, IOriginRouter.DailyVwap[] memory decoded) =
            this.decode(BridgeMsgCodec.encodeDailyVwap(UTC_DAY, rows));
        assertEq(utcDay, UTC_DAY);
        assertEq(decoded.length, rows.length);
        for (uint256 i = 0; i < rows.length; ++i) {
            assertEq(decoded[i].isoCode, rows[i].isoCode);
            assertEq(decoded[i].vwapMinor, rows[i].vwapMinor);
        }
    }

    function test_TheFloorIsTheSmallestDay() public pure {
        assertEq(BridgeMsgCodec.minLengthFor(BridgeMsgCodec.MSG_DAILY_VWAP), BridgeMsgCodec.MIN_LEN_DAILY_VWAP);
        assertEq(BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(1)).length, BridgeMsgCodec.MIN_LEN_DAILY_VWAP);
    }

    function test_RevertWhen_EncodingNoRows() public {
        vm.expectRevert(BridgeMsgCodec.EmptyDailyVwap.selector);
        BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(0));
    }

    function test_RevertWhen_EncodingMoreRowsThanTheReferenceList() public {
        uint256 count = BridgeMsgCodec.MAX_REFERENCE_PRICES + 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.PayloadArrayTooLong.selector, count, BridgeMsgCodec.MAX_REFERENCE_PRICES
            )
        );
        BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(count));
    }

    function test_RevertWhen_DecodingADayWithNoRows() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_DAILY_VWAP,
                BridgeMsgCodec.DAILY_VWAP_HEAD,
                BridgeMsgCodec.MIN_LEN_DAILY_VWAP
            )
        );
        this.decode(abi.encodePacked(uint8(1), BridgeMsgCodec.MSG_DAILY_VWAP, UTC_DAY, uint8(0)));
    }

    function test_RevertWhen_DecodingARowCountTheBodyDoesNotHold() public {
        bytes memory message = BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(2));
        message[6] = bytes1(uint8(3));
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_DAILY_VWAP,
                message.length,
                BridgeMsgCodec.DAILY_VWAP_HEAD + 3 * BridgeMsgCodec.DAILY_VWAP_LEN
            )
        );
        this.decode(message);
    }

    function test_RevertWhen_DecodingBelowTheHead() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_DAILY_VWAP,
                6,
                BridgeMsgCodec.MIN_LEN_DAILY_VWAP
            )
        );
        this.decode(abi.encodePacked(uint8(1), BridgeMsgCodec.MSG_DAILY_VWAP, UTC_DAY));
    }

    function test_RevertWhen_DecodingAnotherBodyVersion() public {
        bytes memory message = BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(1));
        message[0] = bytes1(uint8(2));
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnsupportedBodyVersion.selector, uint8(2)));
        this.decode(message);
    }
}
