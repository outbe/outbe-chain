//! Cycle — deterministic trigger registry and calendar orchestrator.
//!
//! Each `TriggerSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A trigger
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps
//! over multiple hourly slots, ProtocolCycle fires once for the most recent
//! slot. It settles only a contiguous UTC-day transition; days missed during a
//! multi-day halt are forfeited. The current UTC day is never settled.
//!
//! [`triggers::TriggerId::ProtocolCycle`] is aligned to UTC-hour boundaries
//! (`period = 3_600`, `offset = 0` in production). Its handler settles one
//! contiguous completed day or advances `Cycle.active_utc_day` past a forfeited
//! multi-day gap, then invokes the existing Metadosis WWD flow exactly once. A
//! failed step rolls back the whole trigger checkpoint, so the same hourly slot
//! retries on the next block.
//!
//! Each completed-day settlement preserves the existing 5-pool + Metadosis
//! terminal split:
//!
//! 1. Compute `day_emission_limit(day_number_since_genesis(prev_day))`.
//! 2. Allocate over the 5-sink table from `outbe-emissionlimit`.
//! 3. Validator pool: read `outbe_rewards::api::read_daily_fee_sum_raw`
//!    and `read_voters_for_day`; if fees ≥ cap or no voters, return
//!    the validator amount as excess; otherwise call
//!    `add_topup_for_voters`. A fresh settlement returns the amount actually
//!    distributed as Gems, and the undistributed part of the validator
//!    allocation becomes terminal excess. An already-settled top-up is not
//!    minted or terminally credited again.
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
