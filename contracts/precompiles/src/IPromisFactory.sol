// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory - Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when `sender` converts protocol-6 promis to native-18 COEN.
    /// @param coenMinor Native COEN atomic units minted to `sender`.
    event CoenMined(address indexed sender, uint256 coenMinor);

    /// @notice Convert `promisMinor` protocol-6 promis to the same whole-token amount of
    ///         native-18 COEN (burns the caller's confidential promis). The returned
    ///         `coenMinor` and `CoenMined.coenMinor` are native COEN atomic units.
    ///         Authorized by the caller's Promis modify key:
    ///         `mac = HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain promis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`).
    function mineCoen(uint256 promisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);

    /// @notice Convert `promisMinor` promis to confidential Gratis at 1:1 (burns the
    ///         caller's confidential promis, mints gratis). Both tokens are
    ///         enclave-confidential and independently keyed, so the caller supplies
    ///         TWO modify authorizations, each binding `promisMinor` to that ledger's own
    ///         current op-nonce. Fetch each via `outbe_deriveKeys(<ledger>, ...)` +
    ///         `opNonceOf`. The gratis mint records a fresh Fidelity acquisition
    ///         cohort, exactly as any other gratis acquisition does.
    function mineGratis(
        uint256 promisMinor,
        bytes32 promisMac,
        uint64 promisOpNonce,
        bytes32 gratisMac,
        uint64 gratisOpNonce
    ) external returns (uint256 gratisMinor);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
