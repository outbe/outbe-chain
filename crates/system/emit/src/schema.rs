//! Frozen V1 storage schema at `EMIT_ADDRESS`.
//!
//! Tornado-style incremental merkle tree state: no leaves, right nodes,
//! paths, or empty hashes are ever stored. Slot assignment is by declaration
//! order; the circular buffer occupies two slots (4–5).
use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::EMIT_ADDRESS;

/// The only schema version this binary understands; installed lazily by the
/// first accepted burn.
pub const EMIT_SCHEMA_VERSION: u32 = 1;

/// Commitment-tree depth (fixed by the `outbe.emit.mint@1.0.0` circuit).
pub const EMIT_TREE_DEPTH: usize = 20;

/// Tree capacity: `2^20` leaves.
pub const EMIT_TREE_CAPACITY: u32 = 1 << EMIT_TREE_DEPTH;

/// Number of accepted roots retained for mint proofs (the root window).
pub const EMIT_ROOT_WINDOW: u32 = 32;

/// EVM storage layout for the chain's Emit tree (V1).
#[storage_schema]
#[contract(addr = EMIT_ADDRESS)]
pub struct EmitContract {
    // slot 0: initialization/compatibility gate (0 = pristine, 1 = V1)
    pub schema_version: Value<u32>,
    // slot 1: latest commitment root
    pub current_root: Value<B256>,
    // slot 2: next append index
    pub leaf_count: Value<u32>,
    // slot 3: one completed left subtree per level
    pub filled_subtrees: Map<u8, B256>,
    // slots 4–5: last 32 root-producing appends, seeded with the empty root
    pub recent_roots: CircularBuffer<B256>,
    // slot 6: permanent duplicate prevention
    pub commitments: Map<B256, bool>,
    // slot 7: permanent replay prevention
    pub spent_nullifiers: Map<B256, bool>,
}
