// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory - Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when `sender` converts promis to native COEN.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Convert `amount` promis to native COEN at 1:1 (burns the caller's
    ///         confidential promis). Authorized by the caller's Promis modify key:
    ///         `mac = HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain promis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`).
    function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
