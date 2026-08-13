// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// The inbound length floors are hand-derived ABI arithmetic, so each is pinned against the
/// smallest message its encoder can produce.
contract BridgeMsgCodecMinLengthTest is Test {
    function test_RefundFloorIsTheSmallestRealRefund() public pure {
        bytes memory smallest = BridgeMsgCodec.encodeRefundInstructions(
            20_250_101, 0, 1, new address[](0), new uint128[](0), new uint128[](0)
        );
        assertEq(smallest.length, BridgeMsgCodec.MIN_LEN_REFUND_INSTRUCTIONS, "refund floor");
    }

    function test_IssuanceFloorIsTheSmallestRealIssuance() public pure {
        // One series, no winners on this chain — the message a snapshot chain gets when it
        // only needs the series created.
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory series = new BridgeMsgCodec.IssuanceInstructionsPayload[](1);
        series[0].seriesId = "20250101-USD-U";
        series[0].worldwideDay = 20_250_101;

        bytes memory smallest = BridgeMsgCodec.encodeIssuanceInstructions(series);
        assertEq(smallest.length, BridgeMsgCodec.MIN_LEN_ISSUANCE_INSTRUCTIONS, "issuance floor");
    }

    function test_EveryFloorAdmitsWhatTheEncoderProduces() public pure {
        // A floor above the real minimum would reject valid traffic, which is the failure
        // the two tests above cannot see on their own.
        assertLe(
            BridgeMsgCodec.MIN_LEN_REFUND_INSTRUCTIONS,
            BridgeMsgCodec.encodeRefundInstructions(
                20_250_101, 0, 1, new address[](1), new uint128[](1), new uint128[](1)
            )
            .length,
            "a one-bidder refund must pass its floor"
        );
    }
}
