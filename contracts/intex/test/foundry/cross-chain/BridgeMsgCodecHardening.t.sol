// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Thin external wrapper around the `internal pure` encoders so the per-encoder revert paths
///      can be asserted via `vm.expectRevert` from a test contract.
contract BridgeMsgCodecHardeningHarness {
    function encodeBidsBatch(
        uint32 seriesId,
        uint32 srcEid,
        bool isLast,
        uint32 relayGeneration,
        address[] calldata bidders,
        uint16[] calldata quantities,
        uint64[] calldata prices,
        uint32[] calldata timestamps
    ) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeBidsBatch(
            seriesId, srcEid, isLast, relayGeneration, bidders, quantities, prices, timestamps
        );
    }

    function encodeIssuanceInstructions(
        BridgeMsgCodec.IssuanceInstructionsPayload calldata payload
    ) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeIssuanceInstructions(payload);
    }

    function encodeRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint64[] calldata refundedAmounts,
        uint64[] calldata paidAmounts
    ) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeRefundInstructions(seriesId, bidders, refundedAmounts, paidAmounts);
    }

    function decodeRefundInstructions(bytes calldata m)
        external
        pure
        returns (uint32, address[] memory, uint64[] memory, uint64[] memory)
    {
        return BridgeMsgCodec.decodeRefundInstructions(m);
    }

    function decodeBidsBatch(bytes calldata m)
        external
        pure
        returns (uint32, uint32, bool, uint32, address[] memory, uint16[] memory, uint64[] memory, uint32[] memory)
    {
        return BridgeMsgCodec.decodeBidsBatch(m);
    }

    function decodeIssuanceInstructions(bytes calldata m)
        external
        pure
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory)
    {
        return BridgeMsgCodec.decodeIssuanceInstructions(m);
    }
}

/// @dev Defence-in-depth assertions on `BridgeMsgCodec`: encoder-side parallel-array equality
///      checks, inbound `decodeRefundInstructions` cap, and typed `InvalidPayloadLength` on
///      empty-payload entry to the three variable-length decoders.
contract BridgeMsgCodecHardeningTest is Test {
    BridgeMsgCodecHardeningHarness internal harness;

    function setUp() public {
        harness = new BridgeMsgCodecHardeningHarness();
    }

    // --- Encoder parallel-array equality ---

    function test_encodeBidsBatch_arrayLengthMismatch_reverts() public {
        // Decoder rejects parallel-array mismatch; the encoder must surface the same typed error
        // at the source so the LZ send is aborted before paying the fee.
        address[] memory bidders = new address[](2);
        bidders[0] = address(0xB1);
        bidders[1] = address(0xB2);
        uint16[] memory quantities = new uint16[](1); // one short
        quantities[0] = 1;
        uint64[] memory prices = new uint64[](2);
        uint32[] memory timestamps = new uint32[](2);

        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.BidsArrayLengthMismatch.selector, uint256(2), uint256(1), uint256(2), uint256(2)
            )
        );
        harness.encodeBidsBatch(1, 1, true, 1, bidders, quantities, prices, timestamps);
    }

    function test_encodeIssuanceInstructions_arrayLengthMismatch_reverts() public {
        // recipients.length must match quantities.length; encoder reverts before encoding.
        address[] memory recipients = new address[](2);
        recipients[0] = address(0xA1);
        recipients[1] = address(0xA2);
        uint256[] memory quantities = new uint256[](1);
        quantities[0] = 1;

        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.IssuanceInstructionsPayload({
            seriesId: 1,
            issuedIntexCount: 1,
            intexSize: 1,
            intexStrikePrice: 1,
            coenPriceFloor: 1,
            intexCallPeriod: 0,
            settlementTokenAlias: 840,
            callWindowDays: 0,
            callThresholdDays: 0,
            coenPriceCallTrigger: 0,
            recipients: recipients,
            quantities: quantities
        });
        vm.expectRevert(
            abi.encodeWithSelector(BridgeMsgCodec.IssuanceArrayLengthMismatch.selector, uint256(2), uint256(1))
        );
        harness.encodeIssuanceInstructions(payload);
    }

    function test_encodeRefundInstructions_arrayLengthMismatch_reverts() public {
        // bidders / refundedAmounts / paidAmounts must move in lockstep.
        address[] memory bidders = new address[](2);
        bidders[0] = address(0xB1);
        bidders[1] = address(0xB2);
        uint64[] memory refundedAmounts = new uint64[](1); // mismatch
        refundedAmounts[0] = 1;
        uint64[] memory paidAmounts = new uint64[](2);

        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.RefundArrayLengthMismatch.selector, uint256(2), uint256(1), uint256(2)
            )
        );
        harness.encodeRefundInstructions(1, bidders, refundedAmounts, paidAmounts);
    }

    // --- decodeRefundInstructions over-cap symmetric with BIDS / ISSUANCE ---

    function test_decodeRefundInstructions_overCap_revertsRefundBatchTooLarge() public {
        // The outbound encoder caps at MAX_PAYLOAD_ARRAY_LEN; an over-cap inbound payload can only
        // reach the receiver via a peer compromise or a future encoder change. The decoder must
        // reject with the typed RefundBatchTooLarge error so the drop-don't-block handler surfaces
        // a parameterized diagnostic.
        uint256 n = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN + 1;
        address[] memory bidders = new address[](n);
        uint64[] memory refundedAmounts = new uint64[](n);
        uint64[] memory paidAmounts = new uint64[](n);
        for (uint256 i = 0; i < n; ++i) {
            bidders[i] = address(uint160(i + 1));
            refundedAmounts[i] = 1;
            paidAmounts[i] = 0;
        }
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS,
            abi.encode(uint32(42), bidders, refundedAmounts, paidAmounts)
        );

        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.RefundBatchTooLarge.selector, n, uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN)
            )
        );
        harness.decodeRefundInstructions(packet);
    }

    // --- Empty-payload typed revert on the three variable-length decoders ---

    function test_decodeBidsBatch_emptyMsg_revertsInvalidPayloadLength() public {
        // The fixed-length decoders pre-check via _assertExactLength; the variable-length ones must
        // match the same pattern so an empty `_msg` yields a typed error rather than out-of-bounds
        // Panic(0x32) on `_msg[0]`.
        bytes memory empty = "";
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_BIDS_BATCH,
                uint256(0),
                uint256(BridgeMsgCodec.HEADER_LEN)
            )
        );
        harness.decodeBidsBatch(empty);
    }

    function test_decodeIssuanceInstructions_emptyMsg_revertsInvalidPayloadLength() public {
        bytes memory empty = "";
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
                uint256(0),
                uint256(BridgeMsgCodec.HEADER_LEN)
            )
        );
        harness.decodeIssuanceInstructions(empty);
    }

    function test_decodeRefundInstructions_emptyMsg_revertsInvalidPayloadLength() public {
        bytes memory empty = "";
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_REFUND_INSTRUCTIONS,
                uint256(0),
                uint256(BridgeMsgCodec.HEADER_LEN)
            )
        );
        harness.decodeRefundInstructions(empty);
    }
}
