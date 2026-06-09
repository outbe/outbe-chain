// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";

/// @dev Encoder/reader parity and boundary coverage for ONFT1155MsgCodec.
contract ONFT1155MsgCodecGoldenTest is Test {
    bytes32 internal constant SEND_TO = bytes32(uint256(uint160(0x11223344556677889900aABbCcdDEeFF00112233)));
    uint256 internal constant TOKEN_ID = 0x42;
    uint256 internal constant AMOUNT = 1000;

    function test_Encode_NonComposed_LayoutAndLength() public view {
        (bytes memory payload, bool hasCompose) = ONFT1155MsgCodec.encode(SEND_TO, TOKEN_ID, AMOUNT, "");
        assertFalse(hasCompose, "hasCompose for empty composeMsg");
        assertEq(payload.length, ONFT1155MsgCodec.MIN_LEN_TRANSFER, "non-composed length");
        assertEq(uint8(payload[0]), 1, "bodyVersion byte");
        assertEq(_readBytes32(payload, 1), SEND_TO, "sendTo at offset 1");
        assertEq(_readUint256(payload, 33), TOKEN_ID, "tokenId at offset 33");
        assertEq(_readUint256(payload, 65), AMOUNT, "amount at offset 65");
    }

    function test_Encode_Composed_LayoutPinsComposerAtOffset97() public view {
        bytes memory composeMsg = hex"DEADBEEF";
        address sender = msg.sender;
        (bytes memory payload, bool hasCompose) = ONFT1155MsgCodec.encode(SEND_TO, TOKEN_ID, AMOUNT, composeMsg);

        assertTrue(hasCompose, "hasCompose for non-empty composeMsg");
        assertEq(payload.length, uint256(ONFT1155MsgCodec.MIN_LEN_TRANSFER) + 32 + composeMsg.length, "composed length");
        assertEq(uint8(payload[0]), 1, "bodyVersion byte");
        assertEq(
            _readBytes32(payload, 97), ONFT1155MsgCodec.addressToBytes32(sender), "composer = msg.sender at offset 97"
        );
        for (uint256 i = 0; i < composeMsg.length; i++) {
            assertEq(payload[97 + 32 + i], composeMsg[i], "composeMsg byte");
        }
    }

    function test_AssertMinLength_BelowMin_Reverts() public {
        bytes memory tooShort = new bytes(96);
        vm.expectRevert(
            abi.encodeWithSelector(
                ONFT1155MsgCodec.InvalidPayloadLength.selector, uint256(96), uint256(ONFT1155MsgCodec.MIN_LEN_TRANSFER)
            )
        );
        this.exposedAssertMinLength(tooShort);
    }

    function test_FieldAccess_BelowMin_Panics() public {
        bytes memory tooShort = new bytes(96);
        // assertMinLength is the caller's responsibility; without it, fixed-offset slicing on a
        // short payload panics (out-of-bounds calldata slice).
        vm.expectRevert();
        this.exposedAmount(tooShort);
    }

    function test_Composed_BoundaryAt97_NotComposed() public view {
        (bytes memory payload,) = ONFT1155MsgCodec.encode(SEND_TO, TOKEN_ID, AMOUNT, "");
        assertEq(payload.length, 97, "exactly MIN_LEN_TRANSFER");
        assertFalse(this.exposedIsComposed(payload), "97 bytes: not composed");
    }

    function test_Composed_ShortTail_IsComposedButComposerTruncated() public view {
        // 100-byte payload: 97 base + 3 extra (less than the 32-byte composer slot).
        bytes memory payload = new bytes(100);
        payload[0] = bytes1(uint8(1));
        assertTrue(this.exposedIsComposed(payload), "length > 97 marks composed");
        bytes memory tail = this.exposedComposeMsg(payload);
        assertEq(tail.length, 3, "composer is truncated at the codec level");
    }

    function test_Fuzz_RoundTrip_NonComposed(bytes32 to, uint256 id, uint256 amt) public view {
        to = bytes32(uint256(to) & ((1 << 160) - 1));
        (bytes memory payload,) = ONFT1155MsgCodec.encode(to, id, amt, "");
        assertEq(this.exposedSendTo(payload), to, "sendTo round-trip");
        assertEq(this.exposedTokenId(payload), id, "tokenId round-trip");
        assertEq(this.exposedAmount(payload), amt, "amount round-trip");
    }

    function test_AddressBytes32_RoundTrip(address a) public pure {
        bytes32 b = ONFT1155MsgCodec.addressToBytes32(a);
        assertEq(uint256(b) >> 160, 0, "high bits clean");
        assertEq(ONFT1155MsgCodec.bytes32ToAddress(b), a, "address round-trip");
    }

    function test_AssertAddress_DirtyHighBits_Reverts() public {
        bytes32 dirty = bytes32(uint256(0xFF) << 160 | uint256(uint160(address(this))));
        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.MalformedAddress.selector, dirty));
        this.exposedAssertAddress(dirty);
    }

    // --- External wrappers (decoders read calldata) ---

    function exposedSendTo(bytes calldata p) external pure returns (bytes32) {
        return ONFT1155MsgCodec.sendTo(p);
    }

    function exposedTokenId(bytes calldata p) external pure returns (uint256) {
        return ONFT1155MsgCodec.tokenId(p);
    }

    function exposedAmount(bytes calldata p) external pure returns (uint256) {
        return ONFT1155MsgCodec.amount(p);
    }

    function exposedIsComposed(bytes calldata p) external pure returns (bool) {
        return ONFT1155MsgCodec.isComposed(p);
    }

    function exposedComposeMsg(bytes calldata p) external pure returns (bytes memory) {
        return ONFT1155MsgCodec.composeMsg(p);
    }

    function exposedAssertMinLength(bytes calldata p) external pure {
        ONFT1155MsgCodec.assertMinLength(p);
    }

    function exposedAssertAddress(bytes32 v) external pure {
        ONFT1155MsgCodec.assertAddress(v);
    }

    // --- Byte-slice helpers ---

    function _readBytes32(bytes memory data, uint256 offset) internal pure returns (bytes32 out) {
        // bytes memory has a 32-byte length prefix; data + 32 + offset is the byte at `offset`.
        assembly {
            out := mload(add(add(data, 32), offset))
        }
    }

    function _readUint256(bytes memory data, uint256 offset) internal pure returns (uint256) {
        return uint256(_readBytes32(data, offset));
    }
}
