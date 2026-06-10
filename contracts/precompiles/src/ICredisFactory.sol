// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICredisFactory — credis lifecycle orchestrator (0x1009).
/// @notice After the shielded-pool migration the factory consumes a pool
///         commitment via a ZK proof on `requestCredis`, persists the
///         per-position `(denomId, reclaimCommitment)` pair, and inserts the
///         reclaim commitment back into the gratispool when the position
///         completes through `anadosis`.
interface ICredisFactory {
    event CredisRequested(address indexed bundleAccount, uint256 amount);

    /// @notice Arguments for a shielded `requestCredis` spend. Shape matches
    ///         `IGratisPool.SpendArgs` plus the `reclaimCommitment` the user
    ///         pre-computes so the factory can re-insert it into the pool
    ///         when the position completes via `anadosis`.
    ///
    ///         `proof` is the **bare UltraHonk proof body** — i.e. the
    ///         `bb prove` output with the leading
    ///         `[uint32-BE num_public_inputs | N×32B public inputs]` prefix
    ///         stripped. The runtime prepends a fresh prefix built from
    ///         `(merkleRoot, nullifierHash, denomId, receiverBinding)` (in
    ///         circuit declaration order) before calling
    ///         `verify_ultra_honk_keccak`, so the proof is bound atomically
    ///         to the args the runtime is gating — a valid-for-some-other-
    ///         inputs proof cannot be recycled against this call.
    ///
    ///         `receiverBinding` MUST bind `reclaimCommitment` into the
    ///         nonce slot:
    ///         `poseidon(TAG_BINDING, ACTION_REQUEST_CREDIS, bundleAccount,
    ///         chainId, reclaimCommitment)`. The runtime recomputes the
    ///         binding from `args.reclaimCommitment` and rejects with
    ///         `ReceiverBindingMismatch` if it diverges. This closes the
    ///         reclaim-swap front-running attack where a mempool observer
    ///         copies the proof bytes and substitutes their own
    ///         `reclaimCommitment` to capture the eventual
    ///         `unpledgeGratis`.
    struct RequestArgs {
        uint256 merkleRoot;
        uint256 nullifierHash;
        uint8 denomId;
        uint256 receiverBinding;
        bytes proof;
        uint256 reclaimCommitment;
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

    /// @notice Advance the named position by one anadosis installment.
    ///         When the final installment completes, the factory inserts the
    ///         position's stored `reclaimCommitment` into the gratispool so
    ///         the holder of the reclaim secret can later
    ///         `unpledgeGratis(args, destination)`.
    function anadosis(uint256 positionId) external;

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
