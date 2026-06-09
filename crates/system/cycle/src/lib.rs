//! Cycle — generic trigger registry that dispatches periodic workloads
//! from the executor's post-execution block.
//!
//! Each `TriggerSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A trigger
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps
//! over multiple slots (rare clock-jump / restart edge), the trigger
//! fires once for the most recent slot only — pre-genesis design says
//! "Cycle is not responsible for catching up missed days".
//!
//! In v1 the registry contains a single trigger,
//! [`triggers::TriggerId::EmissionLimit1`] (`period = 86_400`,
//! `offset = 0`), whose handler ([`handler::run_emission_limit_daily`])
//! orchestrates the daily 5-pool + Metadosis terminal split:
//!
//! 1. Compute `day_emission_limit(day_number_since_genesis(prev_day))`.
//! 2. Allocate over the 6-sink table from `outbe-emissionlimit`.
//! 3. Validator pool: read `outbe_rewards::api::read_daily_fee_sum_raw`
//!    and `read_voters_for_day`; if fees ≥ cap or no voters, return
//!    the validator amount as excess; otherwise call
//!    `add_topup_for_voters` and treat `fees` as excess.
//! 4. WAA / SRA / CCA / Merchant: call
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
