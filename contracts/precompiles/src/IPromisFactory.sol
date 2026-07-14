// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory — Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when `sender` converts promis to native COEN.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Convert `amount` promis to native COEN at 1:1.
    function mineCoen(uint256 amount) external returns (uint256);

    /// @notice Convert `amount` promis to confidential Gratis at 1:1 (burns
    ///         promis, mints gratis). The gratis mint runs inside the enclave and
    ///         is authorized by the caller's Gratis modify key: `mac =
    ///         HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain gratis op-nonce (fetch via
    ///         `outbe_deriveGratisKeys` + the gratis op-nonce).
    function convertToGratis(uint256 amount, bytes32 mac, uint64 opNonce)
        external
        returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
