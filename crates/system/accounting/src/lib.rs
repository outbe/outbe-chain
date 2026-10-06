//! V2 Phase 1 accounting-progress runtime module.
//!
//! Owns the persistent EVM storage slot
//! `[ACCOUNTING_PROGRESS_ADDRESS] slot 0 = last_accounted_block_number: u64`.
//!
//! ## Scope
//!
//! * [`schema::Accounting`] - single-slot storage facade.
//! * [`state`] - local CRUD helpers around the schema.
//! * [`runtime`] - `record_phase1_progress(ctx, block_number)`, which the V2
//!   executor Phase 1 path calls (the writer is wired), and
//!   `read_last_accounted_block_number(ctx)` for Cycle/Rewards readers.
//!
//! ## Not in scope here
//!
//! * Phase 1 commit logic lives in the executor reorder task.
//! * Phase 2 Cycle gating lives elsewhere.
//!
//! ## System-only
//!
//! `outbe-evm::precompiles::extend_outbe_precompiles` does NOT register
//! `ACCOUNTING_PROGRESS_ADDRESS`. Thus user-issued CALLs to this address do
//! not reach a dispatch routine. They execute as ordinary calls into a no-op
//! account. The only deployed bytecode of that account is the `[0xef]`
//! EIP-161 marker.
//!
//! Only the executor Phase 1 path may write slot 0. Two facts enforce this:
//! the schema facade visibility, and the writer `record_phase1_progress` is
//! the only crate-public mutating entrypoint.

#![forbid(unsafe_code)]

pub mod errors;
pub mod events;
pub mod runtime;
pub mod schema;
pub mod state;

pub use runtime::{read_last_accounted_block_number, record_phase1_progress};
pub use schema::Accounting;
