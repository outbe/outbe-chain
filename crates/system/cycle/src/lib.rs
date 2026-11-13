//! Cycle - deterministic trigger registry and calendar orchestrator.
//!
//! Each `TriggerSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A trigger
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps
//! over multiple hourly slots, ProtocolCycle fires once for the most recent
//! slot. It settles one oldest eligible completed day within the 64-day derived
//! horizon. It never settles the current UTC day.
//!
//! [`triggers::TriggerId::ProtocolCycle`] is aligned to UTC-hour boundaries
//! (`period = 3_600`, `offset = 0` in production). Its handler derives the active
//! UTC day from [`outbe_emission::schema::EmissionContract::emission_days`] and
//! settles one eligible completed day per successful hourly slot.
//! Then the handler invokes the existing Metadosis WWD flow exactly once.
//! A handler revert rolls its atomic work-budget scope back, records the slot, emits
//! `CycleTriggerFailed`, and the walk continues.
//! Local work exhaustion rolls back the whole handler, emits `CycleTriggerOverBudget`,
//! and leaves both watermarks and due dependents pending for the next block.
//! Capacity and dependency deferrals emit `CycleTriggerDeferred` without attempting work.
//! Fatal and other handler errors leave the slot unchanged and return
//! to the caller. CycleTick rejects the block on those errors.
//! A recorded slot waits for its next scheduled window.
//! At a contiguous day transition the slot remains pending until the prior
//! day's last late-vote inclusion window closes. Canonical late participants
//! receive the same daily reward participation weight as base-certificate voters,
//! once per block and for that block's UTC day. Fee decay remains independent.
//!
//! Each completed-day settlement preserves the existing 5-pool + Metadosis
//! terminal split:
//!
//! 1. Compute `emission_limit_of_day(day_number_since_genesis(prev_day))`.
//! 2. Allocate over the 5-sink table from `outbe-emission`.
//! 3. Close participation and calculate the validator top-up through
//!    `outbe_rewards::api::allocate_day_validator_rewards`. The fee-covered part
//!    becomes terminal excess.
//! 4. WAA / SRA / CCA / validator top-up: call
//!    `outbe_agentreward::api::allocate_day_agent_rewards`. All four pools queue
//!    claimable accrual in bounded per-block slices. No-weight excess goes to
//!    Metadosis; rounding residue goes atomically to the Promis Limit.
//! 5. Metadosis terminal credit = metadosis_amount + fee-covered validator
//!    share + pool excess, recorded on `EmissionDay(prev_day)` (`state = Processing`)
//!    and dispatched to `outbe-metadosis`.

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
pub mod triggers;

pub use outbe_rewards::accounting::AccountingProgressError;
pub use runtime::CycleModule;
pub use schema::Accounting;
pub use state::{read_last_accounted_block_number, record_phase1_progress};

#[cfg(test)]
mod tests;
