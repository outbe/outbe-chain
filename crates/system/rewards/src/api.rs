//! Public cross-module API surface for the Rewards module.
//!
//! Cycle seals the votes and fees of a completed day. AgentReward reads the vote weights of a
//! sealed day for the validator reward rate and deletes each weight when its reward is claimed.

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

/// The number of the validators of the sealed `day` and the sum of their votes.
pub fn validator_day_weights(storage: &StorageHandle<'_>, day: u32) -> Result<(u32, U256)> {
    let rewards = Rewards::new(storage.clone());
    require_sealed(&rewards, day)?;
    Ok((
        rewards.day_voter_count.read(&day)?,
        rewards.day_total_votes.read(&day)?,
    ))
}

/// The votes of `voter` in `day`.
pub fn validator_weight(storage: &StorageHandle<'_>, day: u32, voter: Address) -> Result<U256> {
    let rewards = Rewards::new(storage.clone());
    Ok(U256::from(rewards.day_votes.get_nested(&day).read(&voter)?))
}

/// Reads and deletes the votes of `voter` in the sealed `day`, once its reward is claimed or expired.
pub fn take_validator_weight(
    storage: &StorageHandle<'_>,
    day: u32,
    voter: Address,
) -> Result<U256> {
    let rewards = Rewards::new(storage.clone());
    require_sealed(&rewards, day)?;
    let votes = rewards.day_votes.get_nested(&day);
    let weight = votes.read(&voter)?;
    if weight != 0 {
        votes.write(&voter, 0)?;
    }
    Ok(U256::from(weight))
}

/// Deletes the fee sum and the seal block of the sealed `day` once its validator reward is set.
pub fn clear_day_seal(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    let rewards = Rewards::new(storage.clone());
    require_sealed(&rewards, day)?;
    rewards.day_fee_sum_native.write(&day, U256::ZERO)?;
    rewards.day_votes_sealed_at_block.write(&day, 0)
}

/// Deletes the validator count and the vote sum of the sealed `day` with its last weight.
pub fn clear_validator_day(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    let rewards = Rewards::new(storage.clone());
    require_sealed(&rewards, day)?;
    rewards.day_voter_count.write(&day, 0)?;
    rewards.day_total_votes.write(&day, U256::ZERO)
}

fn require_sealed(rewards: &Rewards<'_>, day: u32) -> Result<()> {
    if day == 0 || day > rewards.last_sealed_day.read()? {
        return Err(crate::errors::RewardsError::DayNotSealed.into());
    }
    Ok(())
}
