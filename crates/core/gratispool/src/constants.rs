//! Compile-time parameters of the shielded gratis pool.
//!
//! These values are baked into the `outbe-commitment-nullifier-circuit`
//! Noir program shipped by `outbe-circuits`. Any change here requires a
//! matching change in that circuit (upstream repo) and a regenerated
//! canonical verification key in `outbe-zk-canonical`.

use alloy_primitives::U256;
use outbe_primitives::units::ONE_COEN;

/// Number of supported denominations (length of the ladder returned by
/// [`denomination`]).
pub const DENOMINATION_COUNT: u8 = 3;

/// Gratis denomination amount in 18-decimal base units for the given
/// `denom_id`, or `None` if `denom_id` is outside the supported ladder.
///
/// Valid ids are `1..=DENOMINATION_COUNT`. Id `0` is intentionally invalid
/// so the zero-initialised default of a `denom_id` field rejects rather
/// than silently aliasing the first denomination.
///
/// Each denomination defines a separate anonymity pool. Tornado-style fixed
/// amounts ensure the pledge amount itself is not a unique fingerprint that
/// could link a `pledgeGratis` to a later spend.
///
/// PoC ladder: 100, 1_000, 10_000 GRATIS. Expected to grow once activity
/// warrants larger anonymity sets.
pub fn denomination(denom_id: u8) -> Option<U256> {
    match denom_id {
        1 => Some(U256::from(100u64) * ONE_COEN),
        2 => Some(U256::from(1_000u64) * ONE_COEN),
        3 => Some(U256::from(10_000u64) * ONE_COEN),
        _ => None,
    }
}

// TODO peek correct params

/// Number of leaves the per-denomination Merkle tree can hold.
///
/// Depth 20 → 2^20 ≈ 1.05M commitments per pool. Plenty of headroom for the
/// PoC; the on-chain cost is one `outbe_poseidon` 2-input hash per level on
/// insert (~20 hashes per `deposit_user` / `insert_reclaim`).
pub const MERKLE_DEPTH: u32 = 20;

/// Number of recent root snapshots retained per denomination.
///
/// A spend proof's `merkle_root` public input must match one of the last
/// `ROOT_WINDOW` roots. Matches (~30 roots
/// supports concurrent operations against slightly stale state).
pub const ROOT_WINDOW: u32 = 30;

// ---------------------------------------------------------------------------
// Poseidon domain-separator tags
// ---------------------------------------------------------------------------
//
// Without a tag a hash collision in one context (commitment) could be reused
// in another (nullifier, Merkle inner node).
// Tags are deployment-fixed small distinct field elements; the same constants
// must match the `outbe-commitment-nullifier-circuit` source in
// `outbe-circuits`.

/// `commitment = poseidon(TAG_COMMIT_GRATIS, secret, nullifier_secret, denom_id)`.
pub const TAG_COMMIT_GRATIS: u64 = 0x6E0_001;

/// `nullifier_hash = poseidon(TAG_NULLIFIER_GRATIS, nullifier_secret)`.
pub const TAG_NULLIFIER_GRATIS: u64 = 0x6E0_002;

/// `node = poseidon(TAG_MERKLE_GRATIS, left, right)` — Merkle inner-node hash.
///
/// Hard-coded into the Noir circuit's `merkle_root_from_path` helper.
pub const TAG_MERKLE_GRATIS: u64 = 0x6E0_003;

/// `receiver_binding = poseidon(TAG_BINDING, action_tag, target_address, chain_id, nonce)`.
///
/// Recomputed by the runtime from on-chain inputs and asserted against the
/// proof's public input. A copy-paste attacker who lifts the proof from the
/// mempool cannot reuse it because they cannot re-prove with a different
/// `target_address` without the secret.
pub const TAG_BINDING: u64 = 0x6E0_004;

// ---------------------------------------------------------------------------
// Spend-path action tags
// ---------------------------------------------------------------------------

/// `action_tag` value for `requestCredis` proofs.
///
/// The runtime recomputes `receiver_binding` with this value and
/// `target_address = msg.sender` for `requestCredis` calls.
pub const ACTION_REQUEST_CREDIS: u64 = 1;

/// `action_tag` value for `unpledgeGratis` proofs.
///
/// The runtime recomputes `receiver_binding` with this value and
/// `target_address = destination` for `unpledgeGratis` calls.
pub const ACTION_UNPLEDGE: u64 = 2;

/// Initial value used to pad Merkle-tree subtrees below the current frontier.
///
/// The tree is fully populated from level 0 up: every empty leaf and every
/// empty subtree above an empty leaf is the recursive hash of this constant.
/// `0` is the standard choice — it matches what the Noir circuit witnesses
/// when computing a sibling path past the frontier.
pub const ZERO_LEAF: U256 = U256::ZERO;
