// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

import {IntexNFT1155RouterCodec} from "@contracts/shared/libs/IntexNFT1155RouterCodec.sol";

/// @dev External wrappers so that tests can exercise the `bytes calldata` decoders from memory.
contract CodecHarness {
    function decodeBatch(bytes calldata _message) external pure returns (IntexNFT1155RouterCodec.BatchPayload memory) {
        return IntexNFT1155RouterCodec.decodeBatch(_message);
    }

    function decodeMulti(bytes calldata _message) external pure returns (IntexNFT1155RouterCodec.MultiPayload memory) {
        return IntexNFT1155RouterCodec.decodeMulti(_message);
    }

    function decodeSingle(bytes calldata _message)
        external
        pure
        returns (IntexNFT1155RouterCodec.SinglePayload memory)
    {
        return IntexNFT1155RouterCodec.decodeSingle(_message);
    }

    function assertAddress(bytes32 _value) external pure {
        IntexNFT1155RouterCodec.assertAddress(_value);
    }
}

/// @title IntexNFT1155RouterCodecTest
/// @notice The batch wire codec: single-pass `abi.encode`/`abi.decode` over the named
///         `BatchPayload`/`MultiPayload` structs. Exercises the deep-module behaviour directly:
///         round-trip, version gating, batch-size cap, and the malformed-address reject.
contract IntexNFT1155RouterCodecTest is Test {
    address internal constant OWNER = address(0x5E4D);
    CodecHarness internal harness;

    function setUp() public {
        harness = new CodecHarness();
    }

    function _bytes32(address a) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(a)));
    }

    // --- round-trip ---

    function test_SingleRoundTrip_PreservesFields() public view {
        IntexNFT1155RouterCodec.SinglePayload memory p =
            IntexNFT1155RouterCodec.SinglePayload({owner: OWNER, to: _bytes32(address(0xBEEF)), intexId: 7, units: 3});
        bytes memory encoded = IntexNFT1155RouterCodec.encodeSingle(p);
        assertEq(uint8(encoded[0]), IntexNFT1155RouterCodec.BODY_VERSION_V1);
        assertEq(uint8(encoded[1]), IntexNFT1155RouterCodec.SEND_ONE);
        IntexNFT1155RouterCodec.SinglePayload memory d = harness.decodeSingle(encoded);
        assertEq(d.owner, OWNER);
        assertEq(d.to, p.to);
        assertEq(d.intexId, 7);
        assertEq(d.units, 3);
    }

    function test_DecodeSingle_RevertsOnTrailingBytes() public {
        bytes memory encoded = abi.encodePacked(
            IntexNFT1155RouterCodec.encodeSingle(
                IntexNFT1155RouterCodec.SinglePayload({
                    owner: OWNER, to: _bytes32(address(0xBEEF)), intexId: 7, units: 3
                })
            ),
            bytes32(0)
        );
        vm.expectRevert(IntexNFT1155RouterCodec.MalformedBody.selector);
        harness.decodeSingle(encoded);
    }

    function test_BatchRoundTrip_PreservesFields() public view {
        uint256[] memory intexIds = new uint256[](2);
        intexIds[0] = 20260601;
        intexIds[1] = 20260602;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 7;

        IntexNFT1155RouterCodec.BatchPayload memory p = IntexNFT1155RouterCodec.BatchPayload({
            owner: OWNER, to: _bytes32(address(0xA11CE)), intexIds: intexIds, units: amounts
        });

        bytes memory encoded = IntexNFT1155RouterCodec.encodeBatch(p);

        // Wire prefix is [bodyVersion(1)][msgType(1)].
        assertEq(uint8(encoded[0]), IntexNFT1155RouterCodec.BODY_VERSION_V1, "version byte V1");
        assertEq(uint8(encoded[1]), IntexNFT1155RouterCodec.SEND_BATCH, "msgType SEND_BATCH");

        IntexNFT1155RouterCodec.BatchPayload memory decoded = harness.decodeBatch(encoded);
        assertEq(decoded.owner, OWNER, "owner round-trips");
        assertEq(decoded.to, p.to, "to round-trips");
        assertEq(decoded.intexIds.length, 2, "intexIds length");
        assertEq(decoded.intexIds[0], intexIds[0], "intexId 0");
        assertEq(decoded.intexIds[1], intexIds[1], "intexId 1");
        assertEq(decoded.units[0], amounts[0], "amount 0");
        assertEq(decoded.units[1], amounts[1], "amount 1");
    }

    function test_MultiRoundTrip_PreservesFields() public view {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = _bytes32(address(0xA11CE));
        recipients[1] = _bytes32(address(0xCAFE));
        uint256[] memory intexIds = new uint256[](2);
        intexIds[0] = 20260601;
        intexIds[1] = 20260602;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 3;
        amounts[1] = 4;

        IntexNFT1155RouterCodec.MultiPayload memory p = IntexNFT1155RouterCodec.MultiPayload({
            owner: OWNER, recipients: recipients, intexIds: intexIds, units: amounts
        });

        bytes memory encoded = IntexNFT1155RouterCodec.encodeMulti(p);
        assertEq(uint8(encoded[0]), IntexNFT1155RouterCodec.BODY_VERSION_V1, "version byte V1");
        assertEq(uint8(encoded[1]), IntexNFT1155RouterCodec.SEND_MULTI, "msgType SEND_MULTI");

        IntexNFT1155RouterCodec.MultiPayload memory decoded = harness.decodeMulti(encoded);
        assertEq(decoded.owner, OWNER, "owner round-trips");
        assertEq(decoded.recipients.length, 2, "recipients length");
        assertEq(decoded.recipients[0], recipients[0], "recipient 0");
        assertEq(decoded.recipients[1], recipients[1], "recipient 1");
        assertEq(decoded.intexIds[0], intexIds[0], "intexId 0");
        assertEq(decoded.units[1], amounts[1], "amount 1");
    }

    // --- version gating: an unknown version fails closed ---

    function test_DecodeBatch_Version2_RevertsUnsupportedBodyVersion() public {
        bytes memory unknown = abi.encodePacked(uint8(2), IntexNFT1155RouterCodec.SEND_BATCH, abi.encode(_emptyBatch()));
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.UnsupportedBodyVersion.selector, uint8(2)));
        harness.decodeBatch(unknown);
    }

    function test_DecodeMulti_Version2_RevertsUnsupportedBodyVersion() public {
        bytes memory unknown = abi.encodePacked(uint8(2), IntexNFT1155RouterCodec.SEND_MULTI, abi.encode(_emptyMulti()));
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.UnsupportedBodyVersion.selector, uint8(2)));
        harness.decodeMulti(unknown);
    }

    // --- batch-size cap ---

    function test_DecodeBatch_AtCap_Succeeds() public view {
        IntexNFT1155RouterCodec.BatchPayload memory p = _batchOfSize(IntexNFT1155RouterCodec.MAX_BATCH_SIZE);
        IntexNFT1155RouterCodec.BatchPayload memory decoded =
            harness.decodeBatch(IntexNFT1155RouterCodec.encodeBatch(p));
        assertEq(decoded.intexIds.length, IntexNFT1155RouterCodec.MAX_BATCH_SIZE, "exactly MAX_BATCH_SIZE decodes");
    }

    function test_DecodeBatch_OverCap_RevertsBatchTooLarge() public {
        uint256 over = IntexNFT1155RouterCodec.MAX_BATCH_SIZE + 1;
        bytes memory encoded = IntexNFT1155RouterCodec.encodeBatch(_batchOfSize(over));
        vm.expectRevert(
            abi.encodeWithSelector(
                IntexNFT1155RouterCodec.BatchTooLarge.selector, over, IntexNFT1155RouterCodec.MAX_BATCH_SIZE
            )
        );
        harness.decodeBatch(encoded);
    }

    function test_DecodeMulti_OverCap_RevertsBatchTooLarge() public {
        uint256 over = IntexNFT1155RouterCodec.MAX_BATCH_SIZE + 1;
        bytes memory encoded = IntexNFT1155RouterCodec.encodeMulti(_multiOfSize(over));
        vm.expectRevert(
            abi.encodeWithSelector(
                IntexNFT1155RouterCodec.BatchTooLarge.selector, over, IntexNFT1155RouterCodec.MAX_BATCH_SIZE
            )
        );
        harness.decodeMulti(encoded);
    }

    // --- array-length cross-validation (forged inbound body) ---

    function test_DecodeBatch_MismatchedArrays_RevertsArrayLengthMismatch() public {
        IntexNFT1155RouterCodec.BatchPayload memory p = _emptyBatch();
        p.intexIds = new uint256[](2);
        p.units = new uint256[](1);
        bytes memory encoded = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_BATCH, abi.encode(p)
        );
        vm.expectRevert(IntexNFT1155RouterCodec.ArrayLengthMismatch.selector);
        harness.decodeBatch(encoded);
    }

    // --- schema binding: a body of the wrong type must not decode as this type ---

    function test_DecodeBatch_MultiPayloadBody_RevertsMalformedBody() public {
        // Route a SEND_MULTI-shaped body through the SEND_BATCH decoder. `abi.decode` is permissive.
        // Without the canonical-form check, it would misread the MultiPayload into a garbage
        // BatchPayload (e.g. `to == bytes32(0x60)`). It would then crosschainMint a wrong
        // address.
        bytes memory multiBody = abi.encode(_multiOfSize(1));
        bytes memory packet =
            abi.encodePacked(IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_BATCH, multiBody);
        vm.expectRevert(IntexNFT1155RouterCodec.MalformedBody.selector);
        harness.decodeBatch(packet);
    }

    function test_DecodeMulti_BatchPayloadBody_FailsClosed() public {
        // The reverse direction: a `BatchPayload` body routed as `SEND_MULTI`. `abi.decode` itself
        // reverts on the out-of-bounds offset (a `MultiPayload` needs a third array). Thus it fails
        // closed before the canonical check. The key property is: no misdecode-and-crosschainMint.
        bytes memory batchBody = abi.encode(_batchOfSize(1));
        bytes memory packet =
            abi.encodePacked(IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_MULTI, batchBody);
        vm.expectRevert();
        harness.decodeMulti(packet);
    }

    function test_DecodeBatch_TrailingBytes_RevertsMalformedBody() public {
        // `abi.decode` silently ignores trailing bytes. The canonical check rejects them.
        bytes memory packet = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1,
            IntexNFT1155RouterCodec.SEND_BATCH,
            abi.encode(_batchOfSize(1)),
            hex"deadbeef"
        );
        vm.expectRevert(IntexNFT1155RouterCodec.MalformedBody.selector);
        harness.decodeBatch(packet);
    }

    // --- malformed address helper ---

    function test_AssertAddress_HighBitsSet_RevertsMalformedAddress() public {
        bytes32 bad = bytes32(uint256(1) << 200);
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.MalformedAddress.selector, bad));
        harness.assertAddress(bad);
    }

    function test_AssertAddress_CleanAddress_Passes() public view {
        harness.assertAddress(bytes32(uint256(uint160(address(0xA11CE)))));
    }

    // --- decodeMulti array-length mismatches ---

    function test_DecodeMulti_MismatchIntexIds_RevertsArrayLengthMismatch() public {
        IntexNFT1155RouterCodec.MultiPayload memory p = _emptyMulti();
        p.recipients = new bytes32[](2);
        p.intexIds = new uint256[](1);
        p.units = new uint256[](2);
        bytes memory encoded = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_MULTI, abi.encode(p)
        );
        vm.expectRevert(IntexNFT1155RouterCodec.ArrayLengthMismatch.selector);
        harness.decodeMulti(encoded);
    }

    function test_DecodeMulti_MismatchAmounts_RevertsArrayLengthMismatch() public {
        IntexNFT1155RouterCodec.MultiPayload memory p = _emptyMulti();
        p.recipients = new bytes32[](2);
        p.intexIds = new uint256[](2);
        p.units = new uint256[](1);
        bytes memory encoded = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_MULTI, abi.encode(p)
        );
        vm.expectRevert(IntexNFT1155RouterCodec.ArrayLengthMismatch.selector);
        harness.decodeMulti(encoded);
    }

    // --- fixtures ---

    function _emptyBatch() internal pure returns (IntexNFT1155RouterCodec.BatchPayload memory) {
        return IntexNFT1155RouterCodec.BatchPayload({
            owner: OWNER, to: bytes32(0), intexIds: new uint256[](0), units: new uint256[](0)
        });
    }

    function _emptyMulti() internal pure returns (IntexNFT1155RouterCodec.MultiPayload memory) {
        return IntexNFT1155RouterCodec.MultiPayload({
            owner: OWNER, recipients: new bytes32[](0), intexIds: new uint256[](0), units: new uint256[](0)
        });
    }

    function _batchOfSize(uint256 n) internal pure returns (IntexNFT1155RouterCodec.BatchPayload memory p) {
        p = IntexNFT1155RouterCodec.BatchPayload({
            owner: OWNER, to: _bytes32(address(0xA11CE)), intexIds: new uint256[](n), units: new uint256[](n)
        });
        for (uint256 i = 0; i < n; i++) {
            p.intexIds[i] = i + 1;
            p.units[i] = (i + 1) * 10;
        }
    }

    function _multiOfSize(uint256 n) internal pure returns (IntexNFT1155RouterCodec.MultiPayload memory p) {
        p = IntexNFT1155RouterCodec.MultiPayload({
            owner: OWNER, recipients: new bytes32[](n), intexIds: new uint256[](n), units: new uint256[](n)
        });
        for (uint256 i = 0; i < n; i++) {
            p.recipients[i] = _bytes32(address(uint160(i + 1)));
            p.intexIds[i] = i + 1;
            p.units[i] = (i + 1) * 10;
        }
    }
}
