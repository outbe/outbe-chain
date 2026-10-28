//! Public cross-module API surface for the Rewards module.
//!
//! Exposes the read-only and write entrypoints that other modules
//! (EmissionLimit, AgentReward) call as part of the daily Cycle dispatch
//! chain (`Cycle -> EmissionLimit -> AgentReward -> Rewards`). Before this
//! refactor, `RewardsLifecycle` owned the day-boundary settle, and
//! `on_finalized_metadata` triggered it. With Phase 3, that responsibility
//! moves out of Rewards. Rewards becomes a pure storage + accounting layer.
//! It exposes the data that the new orchestrator needs:
//!
//! * [`read_daily_fee_sum_raw`] - locked-in raw fee total per UTC day.
//!   AgentReward uses it to choose between two actions: forward the validator
//!   pool to Metadosis, or emit a topup.
//! * [`read_voters_for_day`] - ordered (Address, participation count)
//!   pairs for a UTC day. The first-seen-on-day order is deterministic.
//!   AgentReward weights the validator pool by it.

use alloy_primitives::{Address, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::Rewards;

#[cfg(test)]
mod tests;

/// Returns the raw fee total accumulated for the given UTC day. This is
/// the value `on_finalized_metadata` writes per finalized block via
/// `daily_fee_sum_raw[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
/// Full historical details are retained through D+7; expired details can be pruned.
pub fn read_daily_fee_sum_raw(ctx: &BlockRuntimeContext, day: u32) -> Result<U256> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_fee_sum_raw.read(&day)
}

/// Returns the deterministic, first-seen-on-day list of voter
/// participations for `day`. The vector length matches
/// `daily_voter_count[day]`. The index recorded in `daily_voter_at[day][i]`
/// sets the order of the entries. This is the order in which the module first
/// observed a finalized-block bit of each voter for that day.
///
/// Each entry is `(voter_address, participation_count)`. The count
/// is the number of finalized blocks from `day` in which the voter
/// participated. Returns an empty vector if the module recorded no voters
/// for `day`.
/// Full details are guaranteed only through D+7. After expiration, bounded cleanup
/// can expose only the remaining first-seen prefix, then an empty list.
pub fn read_voters_for_day(ctx: &BlockRuntimeContext, day: u32) -> Result<Vec<(Address, u64)>> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let count = rewards.daily_voter_count.read(&day)?;
    let voter_at = rewards.daily_voter_at.get_nested(&day);
    let participation = rewards.daily_participation.get_nested(&day);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let voter = voter_at.read(&i)?;
        let p = participation.read(&voter)?;
        out.push((voter, p));
    }
    Ok(out)
}

/// Whether every known block's participation window for a UTC day has closed.
/// LateFinalizeCredits runs before CycleTick in the same block.
/// At `block_number == last_close`, those close-height credits have already run.
/// This check still requires a later block (`block_number > last_close`).
pub fn day_participation_complete(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<bool> {
    let last_close = ctx
        .storage
        .contract::<Rewards>()
        .daily_last_window_close
        .read(&utc_day)?;
    Ok(last_close == 0 || ctx.block.block_number > last_close)
}

/// Freezes the participation of `day` once its validator pool is credited.
/// A later participant of `day` is refused.
pub fn close_day_participation(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_participation_closed.write(&day, true)
}
