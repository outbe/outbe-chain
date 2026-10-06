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
//! * [`prepare_daily_validator_gem_batch`] - freezes the exact validator Gem
//!   obligations for a UTC day without consulting Oracle state.
//! * [`deliver_oldest_reward_gem_batch`] - delivers one complete FIFO batch
//!   when a fresh canonical price exists.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::Rewards;

mod batch;
mod deliver;
mod prepare;
mod replay;
#[cfg(test)]
mod tests;

pub use deliver::deliver_oldest_reward_gem_batch;
pub use prepare::prepare_daily_validator_gem_batch;

/// Returns the raw fee total accumulated for the given UTC day. This is
/// the value `on_finalized_metadata` writes per finalized block via
/// `daily_fee_sum_raw[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
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

/// Immutable summary of one UTC day's prepared validator reward Gem batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedRewardGemBatch {
    pub reward_utc_day: u32,
    /// Exact COEN-denominated Gem load amount reserved by the daily allocation.
    pub planned_promis_load_amount: U256,
    pub recipient_count: u32,
    pub digest: B256,
}

/// Result of preparing a validator reward Gem obligation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RewardGemPreparationOutcome {
    Prepared(PreparedRewardGemBatch),
    AlreadyPrepared(PreparedRewardGemBatch),
    NoPayableShares(PreparedRewardGemBatch),
}

/// Result of attempting to deliver the oldest prepared reward Gem batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RewardGemDeliveryOutcome {
    Empty,
    PendingRate {
        reward_utc_day: u32,
    },
    Delivered {
        reward_utc_day: u32,
        recipient_count: u32,
        /// Exact COEN-denominated Gem load amount minted by this delivery.
        delivered_promis_load_amount: U256,
    },
}

/// Whether every known block's participation window for a UTC day has closed.
/// Cycle executes before LateFinalizeCredits, so equality is still too early.
/// The final admissible votes at the close height did not execute yet.
pub fn day_participation_complete(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<bool> {
    let last_close = ctx
        .storage
        .contract::<Rewards>()
        .daily_last_window_close
        .read(&utc_day)?;
    Ok(last_close == 0 || ctx.block.block_number > last_close)
}

/// Marks `day` as fully settled so `on_finalized_metadata` rejects any
/// late finalized metadata for that day. The daily Cycle orchestrator owns
/// this call. When the orchestrator finishes dispatching the pools of the day
/// (validator topup, AgentReward pools, Metadosis terminal credit), it calls
/// this to flip the late-after-settle guard. Idempotent.
pub fn mark_day_settled(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_settled.write(&day, true)
}

/// Whether the daily Cycle orchestrator already settled `day` fully
/// (counterpart to [`mark_day_settled`]). The orchestrator reads
/// this before it mints anything. Thus a re-fire for an already-settled day is a
/// no-op, not a double-mint (idempotency).
pub fn is_day_settled(ctx: &BlockRuntimeContext, day: u32) -> Result<bool> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_settled.read(&day)
}
