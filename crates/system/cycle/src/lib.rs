//! Cycle - deterministic trigger registry and calendar orchestrator.
//!
//! Each `TriggerSpec` declares a `period_seconds` and a
//! `start_offset_seconds` phase relative to unix epoch zero. A trigger
//! fires at every slot `t` where `(t - offset) % period == 0`, on the
//! first block whose timestamp is `>= t`. If `block.timestamp` jumps
//! over multiple hourly slots, ProtocolCycle fires once for the most recent
//! slot. It settles only a contiguous UTC-day transition. It forfeits the days
//! missed during a multi-day halt. It never settles the current UTC day.
//!
//! [`triggers::TriggerId::ProtocolCycle`] is aligned to UTC-hour boundaries
//! (`period = 3_600`, `offset = 0` in production). Its handler settles one
//! contiguous completed day or advances `Cycle.active_utc_day` past a forfeited
//! multi-day gap. Then the handler invokes the existing Metadosis WWD flow exactly once.
//! A failed step rolls the trigger checkpoint back and returns the error.
//! CycleTick rejects the block on that error.
//! The same hourly slot does not retry on the next block.
//! At a contiguous day transition the slot remains pending until the prior
//! day's last late-vote inclusion window closes. Canonical late participants
//! receive the same daily GEM participation weight as base-certificate voters,
//! once per block and for that block's UTC day. Fee decay remains independent.
//!
//! Each completed-day settlement preserves the existing 5-pool + Metadosis
//! terminal split:
//!
//! 1. Compute `day_emission_limit(day_number_since_genesis(prev_day))`.
//! 2. Allocate over the 5-sink table from `outbe-emissionlimit`.
//! 3. Validator pool: read `outbe_rewards::api::read_daily_fee_sum_raw`
//!    and `read_voters_for_day`. If fees >= cap or no voters, return
//!    the validator amount as excess. Otherwise, prepare one exact immutable
//!    Rewards Gem batch. The planned total becomes a durable liability and
//!    the undistributed rounding residue becomes terminal excess. The later
//!    `RewardsGemDelivery` system transaction owns delivery.
//! 4. WAA / SRA / CCA: call
//!    `outbe_agentreward::distribute_daily`.
//! 5. Metadosis terminal credit = metadosis_amount + validator_excess +
//!    agent_excess, dispatched through
//!    `outbe_emissionlimit::block::dispatch_terminal_remainder_at` at
//!    the previous-day midnight timestamp.
//! 6. Mark `Rewards.daily_settled[prev_day] = true` to prevent redispatch.

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
