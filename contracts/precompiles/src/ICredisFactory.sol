// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory — credis lifecycle orchestrator (0x1009).
/// @notice After the shielded-pool migration the factory consumes a pool
///         commitment via a ZK proof on `requestCredis`, persists the
///         position's `denomId`, and inserts the caller-supplied reclaim
///         commitment for each installment into the gratispool on every
///         `anadosis` so collateral unlocks one installment at a time.
interface ICredisFactory {
    event CredisRequested(address indexed bundleAccount, uint256 amount);

    /// @notice Arguments for a shielded `requestCredis` spend. Shape matches
    /// `IGratisPool.SpendArgs`. Reclaim is no longer pre-supplied here —
    /// it moved to per-installment `anadosis` calls.
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
    /// `receiverBinding` MUST be
    /// `poseidon(TAG_BINDING, ACTION_REQUEST_CREDIS, bundleAccount,
    /// chainId, 0)` — the context-nonce slot is zero. The runtime
    /// recomputes the binding and rejects with `ReceiverBindingMismatch`
    /// if it diverges, so the loan can only land on `bundleAccount`.
    struct RequestArgs {
        uint256 merkleRoot;
        uint256 nullifierHash;
        uint8 denomId;
        uint256 receiverBinding;
        bytes proof;
    }

    /// @notice Verify a pledge-commitment spend proof and open a credis
    ///         position bound to `bundleAccount`. Gratis stays escrowed in
    ///         `CREDIS_ADDRESS`; the credis position carries the
    ///         denomination's full amount as collateral. Vault sub-call
    ///         delivers the stablecoin to `bundleAccount`.
    /// @return positionId Derived from `nullifierHash` and `bundleAccount`.
    /// @return amountStables Stablecoin amount disbursed (oracle-converted).
    function requestCredis(address asset, address vaultProvider, address bundleAccount, RequestArgs calldata args)
        external
        returns (uint256 positionId, uint256 amountStables);

    /// @notice Advance the named position by one anadosis installment and
    ///         insert `reclaimCommitment` into the gratispool at the anadosis
    ///         (one-decade-down) denomination, worth `denom.amount() / 10`. The
    ///         holder of the reclaim secret can then `unpledgeGratis` that
    ///         installment's share immediately, without waiting for the
    ///         position to complete.
    /// @dev    `reclaimCommitment` MUST be computed with the **anadosis
    ///         denomination id** (one decade below the pledge denom). The
    ///         runtime stores it opaquely and cannot verify the preimage, so a
    ///         note built against the wrong denomination is inserted but
    ///         permanently unspendable. Reverts if `reclaimCommitment` is zero.
    function anadosis(uint256 positionId, uint256 reclaimCommitment) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
