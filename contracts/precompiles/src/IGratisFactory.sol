// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IGratisFactory - Gratis orchestration entry point.
interface IGratisFactory {
    /// @notice Emitted when `sender` converts protocol-6 gratis to native-18 COEN.
    /// @param coenMinor Native COEN atomic units minted to `sender`.
    event CoenMined(address indexed sender, uint256 coenMinor);

    /// @notice Emitted when a user pledges gratis as credis collateral.
    event GratisPledged(
        address indexed account, uint256 principalMinor, address indexed asset, uint256 gratisMinor, bytes32 pledgeNote
    );

    /// @notice Emitted when an unspent pledge is returned to the caller.
    ///         `gratisMinor` is the Gratis credited back.
    event GratisUnpledged(address indexed account, uint256 gratisMinor);

    /// @notice Pledge enough gratis to collateralize `principalMinor`.
    ///         Authorized by the caller's Gratis modify key:
    ///         `mac = HMAC(modifyKey, op-preimage over principalMinor)` where `opNonce`
    ///         MUST equal the caller's current on-chain gratis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `opNonceOf`).
    /// @param principalMinor Stablecoin minor units this pledge must cover.
    /// @param asset         Stablecoin address.
    /// @param maxGratisMinor Slippage cap.
    /// @return pledgeNote The confidential pledge record id. Hand it (and the
    ///         derived pledge secret) to the CCA to request credis.
    function pledgeGratis(uint256 principalMinor, address asset, uint256 maxGratisMinor, bytes32 mac, uint64 opNonce)
        external
        returns (bytes32 pledgeNote);

    /// @notice Directly unpledge an UNSPENT pledge (e.g. credis rejected),
    ///         releasing the full collateral back to `msg.sender`. Authorized by
    ///         the caller's modify key. `principalMinor` is the figure the pledge was
    ///         quoted for and must match the one sealed in the ticket.
    function unpledgeGratis(uint256 principalMinor, bytes32 pledgeNote, bytes32 mac, uint64 opNonce) external;

    /// @notice Convert `gratisMinor` protocol-6 gratis to the same whole-token amount of
    ///         native-18 COEN (burns gratis). The returned `coenMinor` and
    ///         `CoenMined.coenMinor` are native COEN atomic units. Authorized by the
    ///         caller's modify key.
    function mineCoen(uint256 gratisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
