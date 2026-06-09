// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IGratisPool — shielded gratis pool precompile (0x2004).
/// @notice Owns the per-denomination Merkle tree of commitments, the
///         per-denomination root window, and the global nullifier set
///         backing shielded Gratis pledges. The precompile only exposes
///         read-only diagnostics on its ABI; the user-facing pledge,
///         unpledge, requestCredis, and anadosis entrypoints live on
///         IGratisFactory (0x2003) and ICredisFactory (0x1009), which
///         reach the pool through the Rust cross-module API.
interface IGratisPool {
    /// @notice Emitted when any commitment is appended to the tree, whether
    ///         from `pledgeGratis` (via gratisfactory) or from `payAnadosis`
    ///         (via credisfactory).
    event CommitmentInserted(
        uint8 indexed denomId,
        uint256 commitment,
        uint32 leafIndex,
        uint256 newRoot
    );

    /// @notice Emitted when a nullifier is consumed by either spend path.
    ///         `action` is 1 for `requestCredis`, 2 for `unpledgeGratis`.
    event NullifierSpent(uint256 indexed nullifierHash, uint8 action);

    /// @notice Current Merkle root of `denomId`'s commitment tree.
    function currentRoot(uint8 denomId) external view returns (uint256);

    /// @notice Number of commitments appended to `denomId`'s tree so far.
    function leafCount(uint8 denomId) external view returns (uint32);

    /// @notice `true` iff `nullifierHash` has been consumed by any spend.
    function isSpent(uint256 nullifierHash) external view returns (bool);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
