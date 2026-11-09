//! - `schema.rs` - storage schema (the `Rewards` `#[contract]` facade).
//! - `runtime.rs` - runtime helpers (genesis anchor, fingerprint,
//!   day-number conversion).
//! - `lifecycle.rs` - block-boundary entrypoint (`begin_block`).
//! - `precompile.rs` - ABI dispatch.
//! - `api.rs` - public cross-module surface used by the Cycle handler.
//! - `finalized_metadata_hook.rs` - per-finalized-block hook called from
//!   the begin-zone CertifiedParentAccounting phase.
//!
//! Day-boundary settle was removed from this crate (Phase
//! 6 of the Cycle epic). Daily emission orchestration now lives in
//! `outbe-cycle::handler::run_emission_limit_daily`.
//!
//! `pub use` re-exports below preserve the old `contract` / `logic`
//! paths so external callers (RPC, executor, builder, validator set)
//! keep compiling without a sweeping rename. Migrate call sites
//! opportunistically when next touched, then drop the re-exports.
pub mod accounting;
pub mod api;
pub mod block_fees;
pub mod constants;
pub mod errors;
pub mod finalization;
pub mod finalized_metadata_hook;
pub mod lifecycle;
pub mod precompile;
pub mod runtime;
pub mod schema;
#[cfg(test)]
mod test_support;

pub use accounting::{
    read_last_accounted_block_number, record_phase1_progress, Accounting, AccountingProgressError,
};
pub use errors::FinalizationError;
pub use finalization::{
    authenticate_late_credit, run_finalization_and_slashing, run_late_finalize_credits,
    validate_finalized_metadata,
};
pub use lifecycle::RewardsModule;

/// Delegate absent-voter penalties after the executor resolves the canonical snapshot.
pub fn slash_absent_voters(
    storage: outbe_primitives::storage::StorageHandle,
    fb_hash: alloy_primitives::B256,
    absentees: &[alloy_primitives::Address],
) -> outbe_primitives::error::Result<()> {
    outbe_slashindicator::hooks::slash_window_voters(storage, fb_hash, absentees)
}

// Backward-compat aliases for external callers (deprecate when all
// `outbe_rewards::contract::*` / `outbe_rewards::logic::*` call sites
// are migrated).
pub mod contract {
    pub use crate::schema::*;
}
pub mod logic {
    pub use crate::runtime::*;
}
