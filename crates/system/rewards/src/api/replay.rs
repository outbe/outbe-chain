use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use super::batch::{require_reward_gem_queue_consistency, reward_gem_retryable_error};
use super::PreparedRewardGemBatch;
use crate::constants::REWARD_GEM_CURRENCY;
use crate::schema::Rewards;

struct PreparedBatchLiveState {
    recipient_count: u32,
    is_settled: bool,
    sequence_plus_one: u64,
}

impl PreparedBatchLiveState {
    fn read(rewards: &Rewards<'_>, utc_day: u32) -> Result<Self> {
        Ok(Self {
            recipient_count: rewards.reward_gem_recipient_count.read(&utc_day)?,
            is_settled: rewards.daily_topup_settled.read(&utc_day)?,
            sequence_plus_one: rewards.reward_gem_queue_sequence_plus_one.read(&utc_day)?,
        })
    }

    fn retains_live_state(&self) -> bool {
        self.recipient_count != 0 || self.sequence_plus_one != 0
    }

    fn require_no_payable(&self, utc_day: u32) -> Result<()> {
        if !self.is_settled || self.retains_live_state() {
            return Err(reward_gem_retryable_error(format!(
                "validator reward Gem no-payable replay has corrupt state for UTC day {utc_day}"
            )));
        }
        Ok(())
    }

    fn require_delivered(&self, utc_day: u32) -> Result<()> {
        if self.retains_live_state() {
            return Err(reward_gem_retryable_error(format!(
                "validator reward Gem delivered replay retains live state for UTC day {utc_day}"
            )));
        }
        Ok(())
    }
}

pub(super) fn require_identical_reward_gem_batch_replay(
    rewards: &Rewards<'_>,
    summary: &PreparedRewardGemBatch,
    gem_type: u8,
    recipients: &[(Address, U256)],
) -> Result<()> {
    let utc_day = summary.reward_utc_day;
    let stored_digest = rewards.reward_gem_batch_digest.read(&utc_day)?;
    let stored_amount = rewards.reward_gem_planned_load_amount.read(&utc_day)?;
    if stored_digest != summary.digest || stored_amount != summary.planned_promis_load_amount {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem preparation replay contradicts UTC day {utc_day}"
        )));
    }
    require_prepared_reward_gem_batch_replay_consistency(rewards, utc_day, gem_type, recipients)
}

fn require_prepared_reward_gem_batch_replay_consistency(
    rewards: &Rewards<'_>,
    utc_day: u32,
    expected_gem_type: u8,
    expected_recipients: &[(Address, U256)],
) -> Result<()> {
    let state = PreparedBatchLiveState::read(rewards, utc_day)?;
    if expected_recipients.is_empty() {
        return state.require_no_payable(utc_day);
    }

    let expected_count = u32::try_from(expected_recipients.len())
        .map_err(|_| reward_gem_retryable_error("validator reward Gem recipient count overflow"))?;
    require_replay_metadata(rewards, utc_day, expected_gem_type)?;
    if state.is_settled {
        return state.require_delivered(utc_day);
    }
    require_pending_replay_linkage(rewards, utc_day, &state, expected_count)?;
    require_replay_recipients(rewards, utc_day, expected_recipients)
}

fn require_replay_metadata(
    rewards: &Rewards<'_>,
    utc_day: u32,
    expected_gem_type: u8,
) -> Result<()> {
    if rewards.reward_gem_type.read(&utc_day)? != expected_gem_type
        || rewards.reward_gem_issuance_currency.read(&utc_day)? != REWARD_GEM_CURRENCY
        || rewards.reward_gem_reference_currency.read(&utc_day)? != REWARD_GEM_CURRENCY
    {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem preparation replay has corrupt metadata for UTC day {utc_day}"
        )));
    }
    Ok(())
}

fn require_pending_replay_linkage(
    rewards: &Rewards<'_>,
    utc_day: u32,
    state: &PreparedBatchLiveState,
    expected_count: u32,
) -> Result<()> {
    if state.recipient_count != expected_count {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem preparation replay has corrupt recipient count for UTC day {utc_day}"
        )));
    }

    let head = rewards.reward_gem_queue_head.read()?;
    let tail = rewards.reward_gem_queue_tail.read()?;
    require_reward_gem_queue_consistency(rewards, head, tail)?;
    let sequence = state.sequence_plus_one.checked_sub(1).ok_or_else(|| {
        reward_gem_retryable_error(format!(
            "validator reward Gem preparation replay lost FIFO linkage for UTC day {utc_day}"
        ))
    })?;
    if !fifo_holds(head, tail, sequence)
        || rewards.reward_gem_utc_day_by_sequence.read(&sequence)? != utc_day
    {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem preparation replay has corrupt FIFO linkage for UTC day {utc_day}"
        )));
    }
    Ok(())
}

/// Returns `true` when `sequence` is a live position of the reward Gem FIFO:
/// `head <= sequence < tail`.
fn fifo_holds(head: u64, tail: u64, sequence: u64) -> bool {
    head <= tail && head <= sequence && sequence < tail
}

fn require_replay_recipients(
    rewards: &Rewards<'_>,
    utc_day: u32,
    expected_recipients: &[(Address, U256)],
) -> Result<()> {
    let owners = rewards.reward_gem_owner_at.get_nested(&utc_day);
    let loads = rewards.reward_promis_load_at.get_nested(&utc_day);
    for (index, (expected_owner, expected_load)) in expected_recipients.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| {
            reward_gem_retryable_error("validator reward Gem recipient index overflow")
        })?;
        if owners.read(&index)? != *expected_owner || loads.read(&index)? != *expected_load {
            return Err(reward_gem_retryable_error(format!(
                "validator reward Gem preparation replay has corrupt recipient {index} for UTC day {utc_day}"
            )));
        }
    }
    Ok(())
}
