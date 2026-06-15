// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

import {ONFT1155BatchMsgCodec} from "@contracts/shared/libs/ONFT1155BatchMsgCodec.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev External wrappers so the `bytes calldata` decoders can be exercised from memory in tests.
contract CodecHarness {
    function decodeBatch(bytes calldata _message) external pure returns (ONFT1155BatchMsgCodec.BatchPayload memory) {
        return ONFT1155BatchMsgCodec.decodeBatch(_message);
    }

    function decodeMulti(bytes calldata _message) external pure returns (ONFT1155BatchMsgCodec.MultiPayload memory) {
        return ONFT1155BatchMsgCodec.decodeMulti(_message);
    }

    function assertAddress(bytes32 _value) external pure {
        ONFT1155BatchMsgCodec.assertAddress(_value);
    }
}

/// @title ONFT1155BatchMsgCodecTest
/// @notice The batch wire codec migrated from the hand-rolled
///         `abi.encodePacked` packed concat to single-pass `abi.encode`/`abi.decode` with named
///         `BatchPayload`/`MultiPayload` structs and a `V1 -> V2` body-version bump. Exercises the
///         deep-module behaviour directly: round-trip, version gating, batch-size cap, and the
///         malformed-address reject.
contract ONFT1155BatchMsgCodecTest is Test {
    CodecHarness internal harness;

    function setUp() public {
        harness = new CodecHarness();
    }

    function _bytes32(address a) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(a)));
    }

    // --- round-trip ---

    function test_BatchRoundTrip_PreservesFields() public view {
        uint256[] memory tokenIds = new uint256[](2);
        tokenIds[0] = 20260601;
        tokenIds[1] = 20260602;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 7;

        ONFT1155BatchMsgCodec.BatchPayload memory p =
            ONFT1155BatchMsgCodec.BatchPayload({to: _bytes32(address(0xA11CE)), tokenIds: tokenIds, amounts: amounts});

        bytes memory encoded = ONFT1155BatchMsgCodec.encodeBatch(p);

        // Wire prefix is [bodyVersion(1)][msgType(1)].
        assertEq(uint8(encoded[0]), ONFT1155BatchMsgCodec.BODY_VERSION_V2, "version byte V2");
        assertEq(uint8(encoded[1]), ONFT1155BatchMsgCodec.SEND, "msgType SEND");

        ONFT1155BatchMsgCodec.BatchPayload memory decoded = harness.decodeBatch(encoded);
        assertEq(decoded.to, p.to, "to round-trips");
        assertEq(decoded.tokenIds.length, 2, "tokenIds length");
        assertEq(decoded.tokenIds[0], tokenIds[0], "tokenId 0");
        assertEq(decoded.tokenIds[1], tokenIds[1], "tokenId 1");
        assertEq(decoded.amounts[0], amounts[0], "amount 0");
        assertEq(decoded.amounts[1], amounts[1], "amount 1");
    }

    function test_MultiRoundTrip_PreservesFields() public view {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = _bytes32(address(0xA11CE));
        recipients[1] = _bytes32(address(0xCAFE));
        uint256[] memory tokenIds = new uint256[](2);
        tokenIds[0] = 20260601;
        tokenIds[1] = 20260602;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 3;
        amounts[1] = 4;

        ONFT1155BatchMsgCodec.MultiPayload memory p =
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts});

        bytes memory encoded = ONFT1155BatchMsgCodec.encodeMulti(p);
        assertEq(uint8(encoded[0]), ONFT1155BatchMsgCodec.BODY_VERSION_V2, "version byte V2");
        assertEq(uint8(encoded[1]), ONFT1155BatchMsgCodec.SEND_MULTI, "msgType SEND_MULTI");

        ONFT1155BatchMsgCodec.MultiPayload memory decoded = harness.decodeMulti(encoded);
        assertEq(decoded.recipients.length, 2, "recipients length");
        assertEq(decoded.recipients[0], recipients[0], "recipient 0");
        assertEq(decoded.recipients[1], recipients[1], "recipient 1");
        assertEq(decoded.tokenIds[0], tokenIds[0], "tokenId 0");
        assertEq(decoded.amounts[1], amounts[1], "amount 1");
    }

    // --- version gating: stale V1 fails closed ---

    function test_DecodeBatch_StaleV1_RevertsUnsupportedBodyVersion() public {
        // A V1-shaped header (version byte 1) must fail closed rather than misdecode.
        bytes memory staleV1 = abi.encodePacked(uint8(1), ONFT1155BatchMsgCodec.SEND, abi.encode(_emptyBatch()));
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.UnsupportedBodyVersion.selector, uint8(1)));
        harness.decodeBatch(staleV1);
    }

    function test_DecodeMulti_StaleV1_RevertsUnsupportedBodyVersion() public {
        bytes memory staleV1 = abi.encodePacked(uint8(1), ONFT1155BatchMsgCodec.SEND_MULTI, abi.encode(_emptyMulti()));
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.UnsupportedBodyVersion.selector, uint8(1)));
        harness.decodeMulti(staleV1);
    }

    // --- batch-size cap ---

    function test_DecodeBatch_AtCap_Succeeds() public view {
        ONFT1155BatchMsgCodec.BatchPayload memory p = _batchOfSize(ONFT1155BatchMsgCodec.MAX_BATCH_SIZE);
        ONFT1155BatchMsgCodec.BatchPayload memory decoded = harness.decodeBatch(ONFT1155BatchMsgCodec.encodeBatch(p));
        assertEq(decoded.tokenIds.length, ONFT1155BatchMsgCodec.MAX_BATCH_SIZE, "exactly MAX_BATCH_SIZE decodes");
    }

    function test_DecodeBatch_OverCap_RevertsBatchTooLarge() public {
        uint256 over = ONFT1155BatchMsgCodec.MAX_BATCH_SIZE + 1;
        bytes memory encoded = ONFT1155BatchMsgCodec.encodeBatch(_batchOfSize(over));
        vm.expectRevert(
            abi.encodeWithSelector(
                ONFT1155BatchMsgCodec.BatchTooLarge.selector, over, ONFT1155BatchMsgCodec.MAX_BATCH_SIZE
            )
        );
        harness.decodeBatch(encoded);
    }

    function test_DecodeMulti_OverCap_RevertsBatchTooLarge() public {
        uint256 over = ONFT1155BatchMsgCodec.MAX_BATCH_SIZE + 1;
        bytes memory encoded = ONFT1155BatchMsgCodec.encodeMulti(_multiOfSize(over));
        vm.expectRevert(
            abi.encodeWithSelector(
                ONFT1155BatchMsgCodec.BatchTooLarge.selector, over, ONFT1155BatchMsgCodec.MAX_BATCH_SIZE
            )
        );
        harness.decodeMulti(encoded);
    }

    // --- array-length cross-validation (forged inbound body) ---

    function test_DecodeBatch_MismatchedArrays_RevertsArrayLengthMismatch() public {
        ONFT1155BatchMsgCodec.BatchPayload memory p = _emptyBatch();
        p.tokenIds = new uint256[](2);
        p.amounts = new uint256[](1);
        bytes memory encoded =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND, abi.encode(p));
        vm.expectRevert(ONFT1155BatchMsgCodec.ArrayLengthMismatch.selector);
        harness.decodeBatch(encoded);
    }

    // --- schema binding: a body of the wrong type must not decode as this type ---

    function test_DecodeBatch_MultiPayloadBody_RevertsMalformedBody() public {
        // Route a SEND_MULTI-shaped body through the SEND decoder. `abi.decode` is permissive, so
        // without the canonical-form check it would misread the MultiPayload into a garbage
        // BatchPayload (e.g. `to == bytes32(0x60)`) and crosschainMint a wrong address.
        bytes memory multiBody = abi.encode(_multiOfSize(1));
        bytes memory packet =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND, multiBody);
        vm.expectRevert(ONFT1155BatchMsgCodec.MalformedBody.selector);
        harness.decodeBatch(packet);
    }

    function test_DecodeMulti_BatchPayloadBody_FailsClosed() public {
        // The reverse direction: a `BatchPayload` body routed as `SEND_MULTI`. `abi.decode` itself
        // reverts on the out-of-bounds offset (a `MultiPayload` needs a third array), so it fails
        // closed before the canonical check — the key property is no misdecode-and-crosschainMint.
        bytes memory batchBody = abi.encode(_batchOfSize(1));
        bytes memory packet =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND_MULTI, batchBody);
        vm.expectRevert();
        harness.decodeMulti(packet);
    }

    function test_DecodeBatch_TrailingBytes_RevertsMalformedBody() public {
        // `abi.decode` silently ignores trailing bytes; the canonical check rejects them.
        bytes memory packet = abi.encodePacked(
            ONFT1155BatchMsgCodec.BODY_VERSION_V2,
            ONFT1155BatchMsgCodec.SEND,
            abi.encode(_batchOfSize(1)),
            hex"deadbeef"
        );
        vm.expectRevert(ONFT1155BatchMsgCodec.MalformedBody.selector);
        harness.decodeBatch(packet);
    }

    // --- malformed address helper ---

    function test_AssertAddress_HighBitsSet_RevertsMalformedAddress() public {
        bytes32 bad = bytes32(uint256(1) << 200);
        vm.expectRevert(abi.encodeWithSelector(ONFT1155BatchMsgCodec.MalformedAddress.selector, bad));
        harness.assertAddress(bad);
    }

    function test_AssertAddress_CleanAddress_Passes() public view {
        harness.assertAddress(bytes32(uint256(uint160(address(0xA11CE)))));
    }

    // --- decodeMulti array-length mismatches ---

    function test_DecodeMulti_MismatchTokenIds_RevertsArrayLengthMismatch() public {
        ONFT1155BatchMsgCodec.MultiPayload memory p = _emptyMulti();
        p.recipients = new bytes32[](2);
        p.tokenIds = new uint256[](1);
        p.amounts = new uint256[](2);
        bytes memory encoded =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND_MULTI, abi.encode(p));
        vm.expectRevert(ONFT1155BatchMsgCodec.ArrayLengthMismatch.selector);
        harness.decodeMulti(encoded);
    }

    function test_DecodeMulti_MismatchAmounts_RevertsArrayLengthMismatch() public {
        ONFT1155BatchMsgCodec.MultiPayload memory p = _emptyMulti();
        p.recipients = new bytes32[](2);
        p.tokenIds = new uint256[](2);
        p.amounts = new uint256[](1);
        bytes memory encoded =
            abi.encodePacked(ONFT1155BatchMsgCodec.BODY_VERSION_V2, ONFT1155BatchMsgCodec.SEND_MULTI, abi.encode(p));
        vm.expectRevert(ONFT1155BatchMsgCodec.ArrayLengthMismatch.selector);
        harness.decodeMulti(encoded);
    }

    // --- system-wide cap equality ---

    function test_MaxBatchSize_EqualsBridgeMaxPayloadArrayLen() public pure {
        assertEq(ONFT1155BatchMsgCodec.MAX_BATCH_SIZE, uint256(BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN));
    }

    // --- fixtures ---

    function _emptyBatch() internal pure returns (ONFT1155BatchMsgCodec.BatchPayload memory) {
        return
            ONFT1155BatchMsgCodec.BatchPayload({to: bytes32(0), tokenIds: new uint256[](0), amounts: new uint256[](0)});
    }

    function _emptyMulti() internal pure returns (ONFT1155BatchMsgCodec.MultiPayload memory) {
        return ONFT1155BatchMsgCodec.MultiPayload({
            recipients: new bytes32[](0), tokenIds: new uint256[](0), amounts: new uint256[](0)
        });
    }

    function _batchOfSize(uint256 n) internal pure returns (ONFT1155BatchMsgCodec.BatchPayload memory p) {
        p = ONFT1155BatchMsgCodec.BatchPayload({
            to: _bytes32(address(0xA11CE)), tokenIds: new uint256[](n), amounts: new uint256[](n)
        });
        for (uint256 i = 0; i < n; i++) {
            p.tokenIds[i] = i + 1;
            p.amounts[i] = (i + 1) * 10;
        }
    }

    function _multiOfSize(uint256 n) internal pure returns (ONFT1155BatchMsgCodec.MultiPayload memory p) {
        p = ONFT1155BatchMsgCodec.MultiPayload({
            recipients: new bytes32[](n), tokenIds: new uint256[](n), amounts: new uint256[](n)
        });
        for (uint256 i = 0; i < n; i++) {
            p.recipients[i] = _bytes32(address(uint160(i + 1)));
            p.tokenIds[i] = i + 1;
            p.amounts[i] = (i + 1) * 10;
        }
    }
}
