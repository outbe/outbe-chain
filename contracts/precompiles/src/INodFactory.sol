// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    event NodIssued(
        address indexed owner,
        bytes nodId,
        uint256 worldwideDay,
        uint256 leagueId,
        uint256 floorPriceMinor,
        uint256 gratisLoadMinor,
        uint256 entryPriceMinor,
        uint256 costAmountMinor
    );

    event NodBurned(address indexed owner, bytes nodId, uint256 gratisLoadMinor);

    event NodSettled(
        address indexed owner, address indexed payer, bytes nodId, address asset, uint256 amountPaid
    );

    /// @notice Constant-size owner event for one certified OCOMP generation.
    ///         There is deliberately no matching public installation selector.
    event CertifiedNodGenerationInstalled(
        bytes32 indexed activationCallId,
        uint32 indexed worldwideDay,
        uint64 targetGeneration,
        bytes32 namespaceRootBefore,
        uint32 tributeCount,
        uint32 nodCount,
        uint32 bucketCount,
        bytes32 nodRoot,
        bytes32 bucketRoot,
        bytes32 outputManifestRoot,
        uint256 nodAmountTotal,
        uint256 nodGratisConsumed,
        uint64 issuedAt,
        bytes32 stateEventDigest
    );

    /// @notice Pay a Nod's `costAmountMinor` into the reserve vault and mark it
    ///         settled. Callable by anyone, for any Nod, at any point in its
    ///         life; the payer does not have to be the owner. The caller MUST
    ///         grant this precompile an ERC20 allowance of at least
    ///         `costAmountMinor` in `asset` beforehand. A zero-cost Nod is
    ///         settled without any transfer and ignores `asset`.
    /// @return The amount paid.
    function settleNod(bytes calldata nodId, address asset) external returns (uint256);

    /// @notice Burn the caller-owned, settled Nod and mint its gratis load to
    ///         the caller. Authorized by the caller's Gratis modify key: `mac =
    ///         HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain gratis op-nonce. The Nod owner is the
    ///         gratis recipient, so they can always supply this authorization.
    function mineGratis(bytes calldata nodId, uint256 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (uint256);
}
