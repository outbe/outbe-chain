//! Cycle - deterministic task registry and calendar orchestrator.
//!
//! Each `TaskSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A task
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps
//! over multiple hourly slots, ProcessEmissionDay fires once for the most recent
//! slot. It allocates one oldest unallocated completed day within the 64-day derived
//! horizon. It never allocates the current UTC day.
//!
//! [`tasks::TaskId::ProcessEmissionDay`] fires one second into each UTC hour
//! (`period = 3_600`, `offset = 1`). Its handler seals the WWDs whose offering ended and, once a
//! UTC day, opens the WWDs of the day with the records of their emission days. It starts the
//! oldest emission day not started (`start_day_emission_limit`) and, when the offering of a
//! started day ends at 12:00 UTC after it, sets the WAA and SRA rewards and runs its Metadosis.
//! Until the votes of the day are complete it answers not ready, and its dependents wait.
//! A completed handler emits `CycleTaskCompleted`. A handler revert rolls its atomic
//! work-budget scope back, records the slot, logs the cause, and the walk continues.
//! A revert of ProcessEmissionDay fails the block.
//! Local work exhaustion rolls back the whole handler, logs the gas,
//! and leaves both watermarks and due dependents pending for the next block.
//! Capacity and dependency deferrals log the reason without attempting work.
//! Fatal and other handler errors leave the slot unchanged and return
//! to the caller. CycleTick rejects the block on those errors.
//! A recorded slot waits for its next scheduled window.
//! At a contiguous day transition the slot remains pending until the prior
//! day's last late-vote inclusion window closes. Canonical late participants
//! receive the same daily reward participation weight as base-certificate voters,
//! once per block and for that block's UTC day. Fee decay remains independent.
//!
//! The Cycle computes no amount: the record of each emission day holds the Emission Limit and the
//! limit of each part from 00:00 UTC of the day before, and each module writes its result into it.

use alloy_sol_types::sol;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICycle.sol"
);

pub mod call_authority;
pub mod errors;
pub mod handler;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod tasks;

pub use outbe_rewards::accounting::AccountingProgressError;
pub use runtime::CycleModule;
pub use schema::Accounting;
pub use state::{read_last_accounted_block_number, record_phase1_progress};
pub use tasks::CycleWorkerId;

#[cfg(test)]
mod tests;
