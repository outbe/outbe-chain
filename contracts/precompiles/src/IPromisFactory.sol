// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory - Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when `account` converts protocol-6 promis to native-18 COEN.
    /// @param amountMinor Native COEN atomic units minted to `account`.
    event CoenMinedFromPromis(address indexed account, uint256 amountMinor);

    /// @notice Convert `promisMinor` protocol-6 promis to the same whole-token amount of
    ///         native-18 COEN (burns the caller's confidential promis). The returned
    ///         `coenMinor` and `CoenMinedFromPromis.amountMinor` are native COEN atomic units.
    ///         Authorized by the caller's Promis modify key:
    ///         `mac = HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain promis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`).
    function mineCoen(uint256 promisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
