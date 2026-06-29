// SPDX-License-Identifier: MIT
// Adapted from LayerZero Labs' ONFT721MsgCodec (MIT-licensed); copyright (c) LayerZero Labs.
pragma solidity 0.8.30;

/**
 * @title ONFT1155MsgCodec
 * @author Outbe
 * @notice Library for encoding and decoding ONFT1155 LayerZero messages.
 * @dev Kept under MIT (the rest of this repo's first-party code is UNLICENSED) because the
 *      encode/decode API shape and the `bytes32`<->`address` helpers derive from LayerZero's
 *      MIT-licensed ONFT721MsgCodec, whose license notice is retained above. The ERC1155
 *      additions are original to this repo: the leading `bodyVersion` byte, the `amount` field,
 *      and the length / version / address validation.
 *      Wire layout: `[bodyVersion(1)][to(32)][tokenId(32)][amount(32)][composeMsg(variable)]`.
 *      `bodyVersion` lets the format evolve; decoders reject unknown versions.
 */
library ONFT1155MsgCodec {
    /// @notice Active body version emitted by `encode` and required by every decoder helper.
    uint8 internal constant BODY_VERSION_V1 = 1;

    // Body layout offsets (after the leading `bodyVersion` byte).
    uint8 private constant BODY_VERSION_OFFSET = 1;
    uint8 private constant SEND_TO_OFFSET = BODY_VERSION_OFFSET + 32; // 33
    uint8 private constant TOKEN_ID_OFFSET = SEND_TO_OFFSET + 32; // 65
    uint8 private constant AMOUNT_OFFSET = TOKEN_ID_OFFSET + 32; // 97

    /// @notice Minimum encoded length of a non-composed transfer.
    /// @dev Layout: `[bodyVersion(1)][sendTo(32)][tokenId(32)][amount(32)]` = 97 bytes.
    uint16 internal constant MIN_LEN_TRANSFER = 97;

    /// @notice Body decoded with an unsupported `bodyVersion` byte.
    /// @param got The version byte read from the payload.
    error UnsupportedBodyVersion(uint8 got);

    /// @notice Inbound payload is shorter than the minimum encoded transfer.
    /// @param got The actual length of the inbound payload.
    /// @param minimum The minimum required length (`MIN_LEN_TRANSFER`).
    error InvalidPayloadLength(uint256 got, uint256 minimum);

    /// @notice A `bytes32` interpreted as an address has non-zero high bits.
    /// @param got The malformed `bytes32` slot.
    error MalformedAddress(bytes32 got);

    /// @notice Encodes an ONFT1155 message payload.
    /// @dev When `_composeMsg` is non-empty, `addressToBytes32(msg.sender)` is appended after the
    ///      `amount` field and ahead of the compose bytes, hence the `view` mutability.
    /// @param _sendTo The recipient encoded as `bytes32` (low 20 bytes hold the address).
    /// @param _tokenId The ERC1155 token id being transferred.
    /// @param _amount The token amount being transferred.
    /// @param _composeMsg Optional compose payload; empty for a plain transfer.
    /// @return payload The packed message bytes.
    /// @return hasCompose True when `_composeMsg` was non-empty and embedded in `payload`.
    function encode(bytes32 _sendTo, uint256 _tokenId, uint256 _amount, bytes memory _composeMsg)
        internal
        view
        returns (bytes memory payload, bool hasCompose)
    {
        hasCompose = _composeMsg.length > 0;
        payload = hasCompose
            ? abi.encodePacked(BODY_VERSION_V1, _sendTo, _tokenId, _amount, addressToBytes32(msg.sender), _composeMsg)
            : abi.encodePacked(BODY_VERSION_V1, _sendTo, _tokenId, _amount);
    }

    /// @notice Returns the body version byte (offset 0).
    /// @dev Raw accessor: does not assert min length, so the caller must ensure `_msg` is non-empty.
    /// @param _msg The inbound message payload.
    /// @return The `bodyVersion` byte at offset 0.
    function bodyVersion(bytes calldata _msg) internal pure returns (uint8) {
        return uint8(_msg[0]);
    }

    /// @notice Asserts the payload carries the active body version.
    /// @dev Validates `_msg[0] == BODY_VERSION_V1`; reverts `UnsupportedBodyVersion` otherwise.
    /// @param _msg The inbound message payload whose version byte is checked.
    function _assertBodyVersion(bytes calldata _msg) private pure {
        uint8 v = uint8(_msg[0]);
        if (v != BODY_VERSION_V1) revert UnsupportedBodyVersion(v);
    }

    /// @notice Decodes the recipient slot.
    /// @dev Asserts the body version before reading; reverts `UnsupportedBodyVersion` on mismatch.
    /// @param _msg The inbound message payload.
    /// @return The recipient as `bytes32`.
    function sendTo(bytes calldata _msg) internal pure returns (bytes32) {
        _assertBodyVersion(_msg);
        return bytes32(_msg[BODY_VERSION_OFFSET:SEND_TO_OFFSET]);
    }

    /// @notice Decodes the token id.
    /// @dev Asserts the body version before reading; reverts `UnsupportedBodyVersion` on mismatch.
    /// @param _msg The inbound message payload.
    /// @return The ERC1155 token id.
    function tokenId(bytes calldata _msg) internal pure returns (uint256) {
        _assertBodyVersion(_msg);
        return uint256(bytes32(_msg[SEND_TO_OFFSET:TOKEN_ID_OFFSET]));
    }

    /// @notice Decodes the token amount.
    /// @dev Asserts the body version before reading; reverts `UnsupportedBodyVersion` on mismatch.
    /// @param _msg The inbound message payload.
    /// @return The token amount.
    function amount(bytes calldata _msg) internal pure returns (uint256) {
        _assertBodyVersion(_msg);
        return uint256(bytes32(_msg[TOKEN_ID_OFFSET:AMOUNT_OFFSET]));
    }

    /// @notice Reports whether the payload carries a compose message.
    /// @dev True when `_msg` is longer than the fixed transfer body (`AMOUNT_OFFSET` bytes).
    /// @param _msg The inbound message payload.
    /// @return True when compose bytes follow the fixed-length body.
    function isComposed(bytes calldata _msg) internal pure returns (bool) {
        return _msg.length > AMOUNT_OFFSET;
    }

    /// @notice Returns the compose-message tail.
    /// @dev Asserts the body version first. Returns the slice after `AMOUNT_OFFSET`, which still
    ///      includes the appended sender `bytes32` written by `encode`; callers strip it as needed.
    /// @param _msg The inbound message payload.
    /// @return The compose payload bytes following the fixed-length body.
    function composeMsg(bytes calldata _msg) internal pure returns (bytes memory) {
        _assertBodyVersion(_msg);
        return _msg[AMOUNT_OFFSET:];
    }

    /// @notice Reverts `InvalidPayloadLength` if `_msg.length < MIN_LEN_TRANSFER`.
    /// @dev Call this *before* any field access — body-version check assumes the version byte
    ///      exists and the field decoders read fixed-offset slices that would otherwise panic.
    /// @param _msg The inbound message payload to length-check.
    function assertMinLength(bytes calldata _msg) internal pure {
        if (_msg.length < MIN_LEN_TRANSFER) revert InvalidPayloadLength(_msg.length, MIN_LEN_TRANSFER);
    }

    /// @notice Reverts `MalformedAddress(got)` if `_value` cannot be losslessly cast to `address`.
    /// @dev The Solidity address ABI uses the low 20 bytes; the high 12 bytes must be zero.
    /// @param _value The `bytes32` slot to validate as an address.
    function assertAddress(bytes32 _value) internal pure {
        if (uint256(_value) >> 160 != 0) revert MalformedAddress(_value);
    }

    /// @notice Left-pads an `address` into a `bytes32` (low 20 bytes).
    /// @param _addr The address to widen.
    /// @return The address as a zero-padded `bytes32`.
    function addressToBytes32(address _addr) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(_addr)));
    }

    /// @notice Narrows a `bytes32` to its low 20 bytes as an `address`.
    /// @dev Truncates the high 12 bytes without validation; use `assertAddress` first to reject
    ///      a malformed slot.
    /// @param _b The `bytes32` to narrow.
    /// @return The low-20-byte address.
    function bytes32ToAddress(bytes32 _b) internal pure returns (address) {
        return address(uint160(uint256(_b)));
    }
}
