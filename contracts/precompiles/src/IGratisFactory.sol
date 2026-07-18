// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IGratisFactory — Gratis orchestration entry point.
interface IGratisFactory {
    /// @notice Emitted when `sender` converts gratis to native COEN.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Emitted when a user pledges gratis as credis collateral.
    /// `pledgeHandle` is the confidential record id presented later at
    /// `requestCredis`. NOTE: `amount` is public in calldata; only cumulative
    /// balances are encrypted (see the gratis amount-privacy TODO).
    event GratisPledged(address indexed account, uint256 amount, bytes32 pledgeHandle);

    /// @notice Emitted when an unspent pledge is returned to the caller.
    event GratisUnpledged(address indexed account, uint256 amount);

    /// @notice Pledge `amount` gratis as credis collateral. Authorized by the
    ///         caller's Gratis modify key: `mac = HMAC(modifyKey, op-preimage)`
    ///         where `opNonce` MUST equal the caller's current on-chain gratis
    ///         op-nonce (fetch via `outbe_deriveGratisKeys` + the gratis op-nonce).
    /// @return pledgeHandle The confidential pledge record id. Hand it (and the
    ///         derived pledge secret) to the CCA to request credis.
    function pledgeGratis(uint256 amount, bytes32 mac, uint64 opNonce)
        external
        returns (bytes32 pledgeHandle);

    /// @notice Directly unpledge an UNSPENT pledge (e.g. credis rejected),
    ///         releasing the full collateral back to `msg.sender`. Authorized by
    ///         the caller's modify key.
    function unpledgeGratis(uint256 amount, bytes32 pledgeHandle, bytes32 mac, uint64 opNonce)
        external;

    /// @notice Convert `amount` gratis to native COEN at 1:1 (burns gratis).
    ///         Authorized by the caller's modify key.
    function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
