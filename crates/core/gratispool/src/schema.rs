//! Storage schema for the shielded gratis pool.
//!
//! State per denomination:
//!
//! - **Merkle tree of commitments** — an append-only incremental tree of depth
//!   [`MERKLE_DEPTH`][crate::constants::MERKLE_DEPTH]. Tornado-style state
//!   machine: store the `filled_subtrees` frontier per level, the `next_index`
//!   counter, and a ring buffer of the last [`ROOT_WINDOW`][crate::constants::ROOT_WINDOW]
//!   roots. New leaves are appended by hashing the path from the leaf up to the
//!   root, reading the stored frontier on the way.
//! - **Nullifier set** — a global `Set<U256>` of spent nullifiers.
//!   Nullifiers are global, not per-denomination: a single nullifier_secret
//!   could in principle re-occur across denoms, and we want the rejection
//!   regardless. Membership = spent; `Set::insert` returns whether the value
//!   was newly inserted so the spend-path collapses presence check + write
//!   into one atomic op.
//! - **Commitment existence** — a global `Set<U256>` of commitments already
//!   appended to any per-denomination tree. Prevents identical commitments
//!   from being inserted twice across pledge / reclaim paths.
//!
//! Composite keys for per-denomination containers use
//! `keccak256(denom_id || u32 index)` so the macro-generated slot layout stays
//! flat (one `Map<B256, U256>` per concern, regardless of denomination count).

use alloy_primitives::{keccak256, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::GRATIS_POOL_ADDRESS;

/// EVM storage layout for the gratispool precompile at
/// [`GRATIS_POOL_ADDRESS`].
#[storage_schema]
#[contract(addr = GRATIS_POOL_ADDRESS)]
pub struct GratisPoolContract {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 1: per-denomination frontier — `filled_subtrees[denom_id][level]`,
    /// keyed by `keccak256(denom_id || level_be32)`.
    #[attribute(order = 1)]
    pub filled_subtrees: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// slot 2: per-denomination next leaf index — `next_index[denom_id as u32]`.
    #[attribute(order = 2)]
    pub next_index: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// slot 3: per-denomination root ring buffer — `roots[denom_id][slot]`,
    /// keyed by `keccak256(denom_id || slot_be32)` where `slot ∈ [0, ROOT_WINDOW)`.
    #[attribute(order = 3)]
    pub roots: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// slot 4: per-denomination ring-buffer head — `current_root_index[denom_id as u32]`.
    #[attribute(order = 4)]
    pub current_root_index: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// slot 5: global set of spent nullifiers — membership = nullifier has
    /// been consumed by a `verify_and_spend_*` call.
    #[attribute(order = 5)]
    pub nullifier_spent: outbe_primitives::storage::dsl::Set<U256>,

    /// slot 6: global set of existing commitments — membership = commitment
    /// has been appended to some per-denomination tree.
    #[attribute(order = 6)]
    pub commitment_exists: outbe_primitives::storage::dsl::Set<U256>,
}

impl GratisPoolContract<'_> {
    /// `keccak256(denom_id || level_be32)` — slot key for the frontier and the
    /// root ring buffer.
    pub fn level_key(denom_id: u8, position: u32) -> B256 {
        let mut buf = [0u8; 5];
        buf[0] = denom_id;
        buf[1..5].copy_from_slice(&position.to_be_bytes());
        keccak256(buf)
    }
}
