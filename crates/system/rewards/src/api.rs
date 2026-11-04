//! Public cross-module API surface for the Rewards module.
//!
//! Cycle seals completed days with [`seal_votes_and_fees`] and calls [`allocate_day_validator_rewards`] to
//! split the validator budget against native fees. AgentReward creates the job,
//! reads its immutable count/weight through [`sealed_day_inputs`] and recipients
//! through [`reward_voter_at`], then schedules bounded cleanup after settlement.

use alloy_primitives::{Address, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

use crate::schema::Rewards;

#[cfg(test)]
mod tests;

/// Returns the native fee total accumulated for the given UTC day. This is
/// the value `record_votes_and_fees` writes per finalized block via
/// `day_fee_sum_native[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
/// Details remain available until the validator job ends and bounded cleanup runs.
pub fn get_day_fee_sum_native(ctx: &BlockRuntimeContext, day: u32) -> Result<U256> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.day_fee_sum_native.read(&day)
}

/// Returns the deterministic, first-seen-on-day list of voter
/// votes for `day`. The vector length matches
/// `day_voter_count[day]`. The index recorded in `day_voter_at[day][i]`
/// sets the order of the entries. This is the order in which the module first
/// observed a finalized-block bit of each voter for that day.
///
/// Each entry is `(voter_address, vote_count)`. The count
/// is the number of finalized blocks from `day` in which the voter
/// voted. Returns an empty vector if the module recorded no voters
/// for `day`.
/// Details remain available until the validator job ends. During bounded cleanup,
/// cleared indices read as zero; use the indexed APIs for monetary jobs.
pub fn get_day_voters(ctx: &BlockRuntimeContext, day: u32) -> Result<Vec<(Address, u64)>> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let count = rewards.day_voter_count.read(&day)?;
    let voter_at = rewards.day_voter_at.get_nested(&day);
    let votes = rewards.day_votes.get_nested(&day);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let voter = voter_at.read(&i)?;
        let p = votes.read(&voter)?;
        out.push((voter, p));
    }
    Ok(out)
}

/// Whether the votes of the last known block of a UTC day are sealed.
/// LateFinalizeCredits runs before CycleTick in the same block.
/// At `block_number == sealed_at`, those close-height credits have already run.
/// This check still requires a later block (`block_number > sealed_at`).
pub fn is_voting_complete(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<bool> {
    let sealed_at = ctx
        .storage
        .contract::<Rewards>()
        .day_votes_sealed_at_block
        .read(&utc_day)?;
    Ok(sealed_at == 0 || ctx.block.block_number > sealed_at)
}

/// Seals the votes and fees of `day`: sets `last_sealed_day = day`.
/// After this call, a vote or a fee for `day` or an earlier day is refused.
/// Days are sealed in order. A day at or below `last_sealed_day` is refused.
pub fn seal_votes_and_fees(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let last_sealed_day = rewards.last_sealed_day.read()?;
    if day <= last_sealed_day {
        return Err(crate::errors::RewardsError::DayAlreadySealed {
            day,
            last_sealed_day,
        }
        .into());
    }
    rewards.last_sealed_day.write(day)
}

/// Returns the most recent sealed day, or `0` when no day is sealed.
/// Day D is sealed when `D <= get_last_sealed_day()`.
pub fn get_last_sealed_day(storage: &StorageHandle<'_>) -> Result<u32> {
    storage.contract::<Rewards<'_>>().last_sealed_day.read()
}

/// Whether the record of `day` is in the cleanup list. This is true after the
/// validator job of `day` ends or after its allocation fails.
pub fn is_day_cleanup_scheduled(storage: &StorageHandle<'_>, day: u32) -> Result<bool> {
    storage
        .contract::<Rewards<'_>>()
        .cleanup_scheduled
        .read(&day)
}

/// Validator budget split, in protocol units. This does not credit recipients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorRewardAllocation {
    pub topup: U256,
    pub fee_covered: U256,
}

/// Computes the native-fee-adjusted top-up for a completed, sealed day.
pub fn allocate_day_validator_rewards(
    ctx: &BlockRuntimeContext,
    day: u32,
    validator_share: U256,
) -> Result<ValidatorRewardAllocation> {
    crate::runtime::allocate_day_validator_rewards(ctx, day, validator_share)
}

/// Immutable recipient count and total weight for a completed, sealed day.
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
    // Every vote count is bounded by the checked u64 per-voter counter.
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
