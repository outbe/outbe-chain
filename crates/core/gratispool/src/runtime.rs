//! Orchestration logic for the shielded gratis pool.
//!
//! The pool owns the cryptographic state — per-denomination Merkle trees,
//! per-denomination recent-roots window, the global nullifier set, and the
//! global commitment-exists set. It does **not** touch Gratis balances or
//! the per-account pledge ledger; those concerns are the responsibility of
//! whoever orchestrates the call (currently `outbe_gratisfactory` for user
//! pledges and `outbe_credisfactory` for credis position binding /
//! reclaim).
//!
//! Two deposit paths and two spend paths, all over the same Merkle tree and
//! nullifier set:
//!
//! - [`add_commitment`] — append a user-supplied pledge commitment.
//! - [`insert_reclaim`] — CredisFactory-only insert after `payAnadosis`.
//! - [`verify_and_spend_for_credis`] — verify the proof, mark the nullifier
//!   spent, return the denomination amount so credisfactory can size its
//!   position.
//! - [`verify_and_spend_for_unpledge`] — verify the proof, mark the
//!   nullifier spent, return the denomination amount so gratisfactory can
//!   release escrowed Gratis to the destination.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{
    denomination, ACTION_REQUEST_CREDIS, ACTION_UNPLEDGE, TAG_COMMIT_GRATIS, TAG_MERKLE_GRATIS,
    TAG_NULLIFIER_GRATIS,
};
use crate::errors::GratisPoolError;
use crate::precompile::emit_commitment_inserted;
use crate::schema::GratisPoolContract;
use crate::state::{receiver_binding, require_canonical_field};
use crate::verifier;

/// ABI-shape of a spend proof (same shape for both `requestCredis` and
/// `unpledgeGratis`). Reused from the precompile dispatch path; lives on the
/// runtime so the precompile and api crates share one definition.
///
/// `proof` is the **bare UltraHonk proof body** — i.e. the `bb prove` output
/// with the `[u32-BE num_public_inputs | N×32B public inputs]` prefix
/// stripped. The runtime prepends a fresh prefix from `merkle_root`,
/// `nullifier_hash`, `denom_id`, and `receiver_binding` before calling
/// `verify_ultra_honk_keccak`, so the proof is bound atomically to the
/// runtime-authoritative public inputs — no replay against arbitrary args.
///
/// `receiver_binding` itself folds in an application-derived context
/// nonce on top of `(action_tag, target, chain_id)` — see
/// `state::receiver_binding`. For `requestCredis` the prover binds the
/// position's `reclaim_commitment` into that slot; for `unpledgeGratis`
/// the slot is zero.
#[derive(Debug, Clone)]
pub struct SpendArgs {
    pub merkle_root: U256,
    pub nullifier_hash: U256,
    pub denom_id: u8,
    pub receiver_binding: U256,
    pub proof: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Deposit paths
// ---------------------------------------------------------------------------

/// Append `commitment` to `denom_id`'s tree. Pure cryptographic operation —
/// caller is responsible for any matching Gratis-balance movement and pool
/// ledger bookkeeping (see `outbe_gratis::Gratis::pledge_to_pool`).
///
/// Returns `(new_root, leaf_index, denom_amount)`. The amount is returned
/// for caller convenience so it doesn't have to re-derive it from
/// [`denomination`].
pub fn add_commitment(
    storage: StorageHandle<'_>,
    denom_id: u8,
    commitment: U256,
) -> Result<(U256, u32, U256)> {
    let amount = denomination(denom_id).ok_or(GratisPoolError::DenomUnknown)?;

    let mut pool = GratisPoolContract::new(storage.clone());
    let (new_root, leaf_index) = pool.append_leaf(denom_id, commitment)?;
    emit_commitment_inserted(&storage, denom_id, commitment, leaf_index, new_root)?;
    Ok((new_root, leaf_index, amount))
}

// ---------------------------------------------------------------------------
// Spend paths
// ---------------------------------------------------------------------------

/// Verify a `requestCredis` spend proof and consume the nullifier.
///
/// Recomputes `receiver_binding` from `(ACTION_REQUEST_CREDIS, caller,
/// chain_id, nonce)` and asserts equality against the proof's public
/// input. `caller` is the address bound to the resulting Credis position
/// (typically `bundleAccount` as forwarded by credisfactory).
///
/// `nonce` is the application-derived context-binding payload the prover
/// folded into `receiver_binding`. For `credisfactory::requestCredis` the
/// caller passes `args.reclaim_commitment` — this closes the
/// reclaim-swap front-running attack where a mempool observer copies
/// `(args, proof)` and substitutes their own reclaim commitment to
/// capture the eventual `unpledgeGratis`.
///
/// Returns the gratis amount for `denom_id`. The Gratis-side ledger is the
/// caller's responsibility (see `Gratis::bind_pool_to_credis`).
pub fn verify_and_spend_for_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    nonce: U256,
    args: &SpendArgs,
) -> Result<U256> {
    verify_and_spend(storage, caller, ACTION_REQUEST_CREDIS, nonce, args)
}

/// Verify an `unpledgeGratis` spend proof and consume the nullifier.
///
/// Returns the gratis amount for `denom_id`. Caller (gratisfactory) is
/// responsible for releasing the matching amount of Gratis from the credis
/// escrow to `destination` (see `Gratis::unpledge_from_pool`).
///
/// Unpledge is terminal with no follow-up artifact, so the binding nonce
/// slot is pinned to `U256::ZERO` and not exposed at the caller surface.
pub fn verify_and_spend_for_unpledge(
    storage: StorageHandle<'_>,
    destination: Address,
    args: &SpendArgs,
) -> Result<U256> {
    verify_and_spend(storage, destination, ACTION_UNPLEDGE, U256::ZERO, args)
}

// ---------------------------------------------------------------------------
// Shared spend-path body
// ---------------------------------------------------------------------------

fn verify_and_spend(
    storage: StorageHandle<'_>,
    target: Address,
    action_tag: u64,
    nonce: U256,
    args: &SpendArgs,
) -> Result<U256> {
    let amount = denomination(args.denom_id).ok_or(GratisPoolError::DenomUnknown)?;

    // 0. Reject non-canonical (>= scalar-field modulus) field-element inputs.
    //    The Barretenberg verifier reduces public inputs modulo `p`, so
    //    `N` and `N + p` verify identically; if the runtime then used a raw
    //    `U256` as state (the `nullifier_spent` key in particular) those two
    //    representatives would be distinct keys for the same nullifier and a
    //    note could be spent multiple times. Canonicalise before any use.
    require_canonical_field(args.merkle_root)?;
    require_canonical_field(args.nullifier_hash)?;
    require_canonical_field(args.receiver_binding)?;

    // 1. The proof's receiver_binding public input must match what the
    //    runtime recomputes for this call.
    let chain_id = storage.chain_id()?;
    let expected_binding = receiver_binding(action_tag, target, chain_id, nonce)?;
    if expected_binding != args.receiver_binding {
        return Err(GratisPoolError::ReceiverBindingMismatch.into());
    }

    // 2. The proof's merkle_root must be a recently-recorded root for this
    //    denomination. Older roots have dropped out of the ring buffer.
    let pool = GratisPoolContract::new(storage);
    if !pool.has_root_in_window(args.denom_id, args.merkle_root)? {
        return Err(GratisPoolError::RootStale.into());
    }

    // 3. The proof must verify against the VK *and* against runtime-authoritative
    //    public inputs. The verifier prepends them to the proof body,
    //    so the binding is atomic — a proof valid for some other (root, nullifier, denom,
    //    binding, tag-triple) tuple cannot be replayed against this call's
    //    `args`. Order matches circuit declaration order in the upstream
    //    `outbe-commitment-nullifier-circuit::main` (see `outbe-circuits`).
    let public_inputs: [U256; verifier::NUM_PUBLIC_INPUTS] = [
        args.merkle_root,
        args.nullifier_hash,
        U256::from(args.denom_id),
        args.receiver_binding,
        U256::from(TAG_COMMIT_GRATIS),
        U256::from(TAG_NULLIFIER_GRATIS),
        U256::from(TAG_MERKLE_GRATIS),
    ];
    if !verifier::verify(&public_inputs, &args.proof) {
        return Err(GratisPoolError::ProofInvalid.into());
    }

    // 4. Consume the nullifier last. `Set::insert` returns `false` if it was
    //    already present, which is the double-spend rejection.
    if !pool.nullifier_spent.insert(args.nullifier_hash)? {
        return Err(GratisPoolError::NullifierSpent.into());
    };

    Ok(amount)
}
