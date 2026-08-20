//! Cycle — generic trigger registry that dispatches periodic workloads
//! from the executor's post-execution block.
//!
//! Each `TriggerSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A trigger
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps over
//! multiple slots (rare clock-jump / restart edge), the dispatcher processes
//! one pending slot per block until its canonical checkpoint catches up.
//!
//! Before the height-gated hourly flow activates,
//! [`triggers::TriggerId::EmissionLimit1`] (`period = 86_400`, `offset = 0`)
//! orchestrates the daily 5-pool + Metadosis terminal split. After activation,
//! the legacy Metadosis triggers remain audit-only no-ops and
//! [`triggers::TriggerId::MetadosisHourly`] owns the same optional daily
//! settlement followed by WWD advancement and READY processing:
//!
//! 1. Compute `day_emission_limit(day_number_since_genesis(prev_day))`.
//! 2. Allocate over the 5-sink table from `outbe-emissionlimit`.
//! 3. Validator pool: read `outbe_rewards::api::read_daily_fee_sum_raw`
//!    and `read_voters_for_day`; if fees ≥ cap or no voters, return
//!    the validator amount as excess; otherwise call
//!    `add_topup_for_voters` and treat `fees` as excess.
//! 4. WAA / SRA / CCA: call
//!    `outbe_agentreward::distribute_daily`.
//! 5. Metadosis terminal credit = metadosis_amount + validator_excess +
//!    agent_excess, dispatched through
//!    `outbe_emissionlimit::block::dispatch_terminal_remainder_at` at
//!    the previous-day midnight timestamp.
//! 6. Mark `Rewards.daily_settled[prev_day] = true` so late finalized
//!    metadata for the day is rejected by `on_finalized_metadata`.

use alloy_sol_types::sol;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICycle.sol"
);

pub mod handler;
pub mod lifecycle;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod triggers;

#[cfg(test)]
mod tests;
