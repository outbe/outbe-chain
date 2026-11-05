//! Public cross-module API surface for the Rewards module.
//!
//! Cycle closes completed days and calls [`allocate_day_validator_rewards`] to
//! split the validator budget against native fees. AgentReward creates the job,
//! reads its immutable count/weight through [`sealed_day_inputs`] and recipients
//! through [`reward_voter_at`], then schedules bounded cleanup after settlement.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

use crate::schema::Rewards;

#[cfg(test)]
mod tests;

/// Returns the raw fee total accumulated for the given UTC day. This is
/// the value `record_votes_and_fees` writes per finalized block via
/// `day_fee_sum_native[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
/// Details remain available until the validator job ends and bounded cleanup runs.
pub fn get_day_fee_sum_native(ctx: &BlockRuntimeContext, day: u32) -> Result<U256> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.day_fee_sum_native.read(&day)
}

/// Returns the deterministic, first-seen-on-day list of voter
/// participations for `day`. The vector length matches
/// `day_voter_count[day]`. The index recorded in `day_voter_at[day][i]`
/// sets the order of the entries. This is the order in which the module first
/// observed a finalized-block bit of each voter for that day.
///
/// Each entry is `(voter_address, participation_count)`. The count
/// is the number of finalized blocks from `day` in which the voter
/// participated. Returns an empty vector if the module recorded no voters
/// for `day`.
/// Details remain available until the validator job ends. During bounded cleanup,
/// cleared indices read as zero; use the indexed APIs for monetary jobs.
pub fn get_day_voters(ctx: &BlockRuntimeContext, day: u32) -> Result<Vec<(Address, u64)>> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let count = rewards.day_voter_count.read(&day)?;
    let voter_at = rewards.day_voter_at.get_nested(&day);
    let participation = rewards.day_votes.get_nested(&day);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let voter = voter_at.read(&i)?;
        let p = participation.read(&voter)?;
        out.push((voter, p));
    }
    Ok(out)
}

/// Whether the day's voting boundary and all open windows through that day have closed.
/// LateFinalizeCredits runs before CycleTick in the same block.
/// At `block_number == last_close`, those close-height credits have already run.
/// This check still requires a later block (`block_number > last_close`).
pub fn is_voting_complete(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<bool> {
    let rewards = ctx.storage.contract::<Rewards>();
    let last_close = rewards.day_votes_sealed_at_block.read(&utc_day)?;
    if last_close != 0 && ctx.block.block_number <= last_close {
        return Ok(false);
    }
    // A catch-up jump must not freeze the old parent's still-open votes after a halt.
    // Canonical late-credit processing closes older bindings; at most K remain live.
    let first_open = ctx
        .block
        .block_number
        .saturating_sub(outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K);
    for number in first_open..ctx.block.block_number {
        let hash = rewards.pending_fb_hash_at.read(&number)?;
        if hash == B256::ZERO {
            continue;
        }
        let day = rewards.pending_reward_day.read(&hash)?;
        if day == 0 {
            return Err(crate::errors::RewardsError::LateCreditMissingCanonicalUtcDay.into());
        }
        if day <= utc_day {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Returns the shared immutable-day watermark. Zero means no day is sealed.
pub fn get_last_sealed_day(storage: &StorageHandle<'_>) -> Result<u32> {
    Rewards::new(storage.clone()).last_sealed_day.read()
}

/// Freezes votes, fees, and agent weights through a completed calendar day.
/// Forward jumps preserve Cycle's bounded catch-up policy without allocating skipped days.
pub fn seal_votes_and_fees(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    crate::runtime::seal_votes_and_fees(ctx, day)
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
        rewards.day_voter_count.read(&day)?,
        rewards.day_total_votes.read(&day)?,
    ))
}

/// Reads one sealed recipient without constructing the day population.
pub fn reward_voter_at(ctx: &BlockRuntimeContext, day: u32, index: u32) -> Result<(Address, U256)> {
    let (count, total) = sealed_day_inputs(ctx, day)?;
    if index >= count {
        return Err(crate::errors::RewardsError::VoterIndexOutOfBounds.into());
    }
    let rewards = ctx.storage.contract::<Rewards>();
    let voter = rewards.day_voter_at.get_nested(&day).read(&index)?;
    // Every participation count is bounded by the checked u64 per-voter counter.
    let weight = U256::from(rewards.day_votes.get_nested(&day).read(&voter)?);
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
pub fn cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    crate::runtime::cleanup(ctx)
}
