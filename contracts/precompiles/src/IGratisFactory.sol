// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IGratisFactory — shielded Gratis orchestration entry point (0x2003).
/// @notice Bridges the public Gratis token (0x1003) to the shielded gratis
///         pool (0x2004). pledgeGratis moves the caller's Gratis into the
///         credis escrow and appends the supplied commitment to the pool;
///         unpledgeGratis consumes a pool commitment via a ZK proof and
///         releases the matching escrowed amount back to the caller. The
///         destination is fixed to msg.sender so the pool cannot be used as
///         a Gratis-transfer channel between addresses — the caller must
///         hold the secret AND own the per-pledger ledger entry.
interface IGratisFactory {
    /// @notice Emitted when gratis is minted to `account`.
    event GratisMined(address indexed account, uint256 amount);

    /// @notice Emitted when `sender` converts gratis to native COEN.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Emitted when a user adds a shielded pledge to the pool.
    event GratisPledged(address indexed account, uint8 indexed denomId, uint256 commitment);

    /// @notice Emitted when a pool commitment is spent via unpledge and the
    ///         denomination amount lands back at the caller.
    event GratisUnpledged(address indexed account, uint8 indexed denomId, uint256 amount);

    /// @notice Spend-proof payload. Same shape as `ICredisFactory.RequestArgs`
    /// minus the reclaim commitment (unpledge is terminal; no
    /// follow-up reclaim is registered).
    ///
    /// `proof` is the **bare UltraHonk proof body** — i.e. the
    /// `bb prove` output with the leading
    /// `[uint32-BE num_public_inputs | N×32B public inputs]` prefix
    /// stripped. The runtime prepends a fresh prefix built from
    /// `(merkleRoot, nullifierHash, denomId, receiverBinding)` (in
    /// circuit declaration order) before calling
    /// `verify_ultra_honk_keccak`, so the proof is bound atomically
    /// to the args the runtime is gating — a valid-for-some-other-
    /// inputs proof cannot be recycled against this call.
    ///
    /// `receiverBinding` itself folds in an application context
    /// nonce on top of `(actionTag, target, chainId)`. For
    /// `unpledgeGratis` the nonce is `0` (terminal — no follow-up
    /// artifact). The prover MUST compute the binding as
    /// `poseidon(TAG_BINDING, ACTION_UNPLEDGE, msg.sender, chainId, 0)`.
    struct SpendArgs {
        uint256 merkleRoot;
        uint256 nullifierHash;
        uint8 denomId;
        uint256 receiverBinding;
        bytes proof;
    }

    /// @notice Add a shielded Gratis pledge.
    /// @param denomId Denomination index. Reverts if out of range.
    /// @param commitment Poseidon commitment. Reverts if already in the pool.
    /// @return newRoot The pool's new Merkle root for `denomId` after the
    ///         commitment is appended. The caller uses this directly as the
    ///         `merkleRoot` public input of any spend proof against this
    ///         pledge, avoiding a follow-up read of `currentRoot`.
    function pledgeGratis(uint8 denomId, uint256 commitment) external returns (uint256 newRoot);

    /// @notice Spend a pool commitment, releasing the denomination amount of
    ///         Gratis back to `msg.sender`. The receiver_binding public
    ///         input must bind to `msg.sender` — there is no way to direct
    ///         the released Gratis to a different address.
    function unpledgeGratis(SpendArgs calldata args) external;

    /// @notice Convert `amount` gratis to native COEN at 1:1. Burns the gratis,
    ///         records the Fidelity sale cohort, and mints the native COEN to
    ///         `msg.sender`. Returns the minted native amount.
    function mineCoen(uint256 amount) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
