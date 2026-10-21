//! Local settled-day queue transitions and bounded daily-record mutations.

use alloy_primitives::{Address, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::{errors::RewardsError, schema::Rewards};

pub(crate) fn cleanup_positions(rewards: &Rewards<'_>) -> Result<(u64, u64)> {
    let head = rewards.settled_day_cleanup_head.read()?;
    let tail = rewards.settled_day_cleanup_tail.read()?;
    if head > tail {
        return Err(RewardsError::InvalidSettledDayCleanupQueue("head exceeds tail").into());
    }
    Ok((head, tail))
}

pub(crate) fn mark_day_settled(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    ctx.storage.with_checkpoint(|| {
        let rewards = ctx.storage.contract::<Rewards>();
        if rewards.daily_settled.read(&day)? {
            return Ok(());
        }
        if !rewards.daily_participation_closed.read(&day)?
            || !crate::api::day_participation_complete(ctx, day)?
        {
            return Err(RewardsError::DaySettlementNotReady.into());
        }
        let (_, tail) = cleanup_positions(&rewards)?;
        let next = tail
            .checked_add(1)
            .ok_or(RewardsError::SettledDayCleanupQueueOverflow)?;
        if rewards.settled_day_cleanup_day_at.read(&tail)? != 0 {
            return Err(RewardsError::InvalidSettledDayCleanupQueue("occupied tail").into());
        }
        rewards.settled_day_cleanup_day_at.write(&tail, day)?;
        rewards.settled_day_cleanup_tail.write(next)?;
        rewards.daily_settled.write(&day, true)
    })
}

/// Removes the tail of the voter list, leaving an enumerable prefix for resumption.
/// The caller checkpoints the entire slice, including queue advancement.
pub(crate) fn prune_day_slice(rewards: &Rewards<'_>, head: u64, day: u32) -> Result<()> {
    let count = rewards.daily_voter_count.read(&day)?;
    let remaining = count.saturating_sub(crate::runtime::SETTLED_DAY_CLEANUP_VOTER_LIMIT);
    let voters = rewards.daily_voter_at.get_nested(&day);
    let participation = rewards.daily_participation.get_nested(&day);
    for index in (remaining..count).rev() {
        let voter = voters.read(&index)?;
        if participation.read(&voter)? == 0 {
            return Err(
                RewardsError::InvalidSettledDayCleanupQueue("missing voter participation").into(),
            );
        }
        participation.write(&voter, 0)?;
        voters.write(&index, Address::ZERO)?;
    }
    rewards.daily_voter_count.write(&day, remaining)?;
    if remaining == 0 {
        let next = head
            .checked_add(1)
            .ok_or(RewardsError::SettledDayCleanupQueueOverflow)?;
        rewards.daily_fee_sum_raw.write(&day, U256::ZERO)?;
        rewards.daily_last_window_close.write(&day, 0)?;
        rewards.settled_day_cleanup_day_at.write(&head, 0)?;
        rewards.settled_day_cleanup_head.write(next)?;
    }
    Ok(())
}
