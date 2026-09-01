// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory - Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when `sender` converts protocol-6 promis to native-18 COEN.
    /// @param amount Native COEN atomic units minted to `sender`.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Convert `amount` protocol-6 promis to the same whole-token amount of
    ///         native-18 COEN (burns the caller's confidential promis). The return
    ///         value and `CoenMined.amount` are native COEN atomic units.
    ///         Authorized by the caller's Promis modify key:
    ///         `mac = HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain promis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`).
    function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
