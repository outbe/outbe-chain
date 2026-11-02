//! Public cross-module API surface for the Rewards module.
//!
//! Cycle closes completed days and calls [`allocate_day_validator_rewards`] to
//! split the validator budget against native fees. AgentReward creates the job,
//! reads its immutable count/weight through [`sealed_day_inputs`] and recipients
//! through [`reward_voter_at`], then schedules bounded cleanup after settlement.

use alloy_primitives::{Address, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::Rewards;

#[cfg(test)]
mod tests;

/// Returns the raw fee total accumulated for the given UTC day. This is
/// the value `on_finalized_metadata` writes per finalized block via
/// `daily_fee_sum_raw[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
/// Details remain available until the validator job ends and bounded cleanup runs.
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
/// Details remain available until the validator job ends. During bounded cleanup,
/// cleared indices read as zero; use the indexed APIs for monetary jobs.
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

/// Validator budget split, in protocol units. This does not credit recipients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorRewardAllocation {
    pub topup: U256,
    pub fee_covered: U256,
}

/// Computes the native-fee-adjusted top-up for a completed, closed day.
pub fn allocate_day_validator_rewards(
    ctx: &BlockRuntimeContext,
    day: u32,
    validator_share: U256,
) -> Result<ValidatorRewardAllocation> {
    crate::runtime::allocate_day_validator_rewards(ctx, day, validator_share)
}

/// Immutable recipient count and total weight for a completed, closed day.
/// The caller must finish its monetary job before scheduling cleanup.
pub fn sealed_day_inputs(ctx: &BlockRuntimeContext, day: u32) -> Result<(u32, U256)> {
    crate::runtime::require_sealed_day(ctx, day)?;
    let rewards = ctx.storage.contract::<Rewards>();
    Ok((
        rewards.daily_voter_count.read(&day)?,
        rewards.daily_total_participation.read(&day)?,
    ))
}

/// Reads one sealed recipient without constructing the day population.
pub fn reward_voter_at(ctx: &BlockRuntimeContext, day: u32, index: u32) -> Result<(Address, U256)> {
    let (count, total) = sealed_day_inputs(ctx, day)?;
    if index >= count {
        return Err(crate::errors::RewardsError::VoterIndexOutOfBounds.into());
    }
    let rewards = ctx.storage.contract::<Rewards>();
    let voter = rewards.daily_voter_at.get_nested(&day).read(&index)?;
    // Every participation count is bounded by the checked u64 per-voter counter.
    let weight = U256::from(rewards.daily_participation.get_nested(&day).read(&voter)?);
    if voter.is_zero() || (!total.is_zero() && weight.is_zero()) {
        return Err(crate::errors::RewardsError::InvalidVoterEntry.into());
    }
    Ok((voter, weight))
}

/// Queues a day once, only after its validator job has finished or was abandoned.
pub fn enqueue_day_cleanup(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    crate::runtime::enqueue_day_cleanup(ctx, day)
}

/// Deletes at most 256 voter entries and four headers per block, independently
/// of the monetary worker's 256-recipient budget. Failure rolls back this pass.
pub fn process_day_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::runtime::process_day_cleanup(ctx)
}
