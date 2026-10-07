use alloy_primitives::{Address, U256};
use outbe_gemfactory::GemTypes;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    time::{date_key_to_utc_timestamp, next_date_key},
};

use super::batch::{
    require_reward_gem_currency, require_reward_gem_queue_consistency, reward_gem_batch_digest,
    reward_gem_retryable_error, RewardGemBatchTerms,
};
use super::replay::require_identical_reward_gem_batch_replay;
use super::{day_participation_complete, PreparedRewardGemBatch, RewardGemPreparationOutcome};
use crate::constants::REWARD_GEM_CURRENCY;
use crate::schema::Rewards;

/// Calculates and stores one exact validator reward Gem obligation without
/// consulting a live Oracle price or minting a Gem. The first preparation owns
/// the immutable FIFO append. An exact replay returns the stored summary.
pub fn prepare_daily_validator_gem_batch(
    ctx: &BlockRuntimeContext,
    utc_day: u32,
    validator_topup_amount: U256,
    voters: &[(Address, u64)],
) -> Result<RewardGemPreparationOutcome> {
    ctx.with_checkpoint(|| {
        prepare_daily_validator_gem_batch_inner(ctx, utc_day, validator_topup_amount, voters)
    })
}

fn prepare_daily_validator_gem_batch_inner(
    ctx: &BlockRuntimeContext,
    utc_day: u32,
    validator_topup_amount: U256,
    voters: &[(Address, u64)],
) -> Result<RewardGemPreparationOutcome> {
    if !day_participation_complete(ctx, utc_day)? {
        return Err(PrecompileError::Fatal(
            "GEM preparation before reward participation windows close".into(),
        ));
    }
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let gem_type = reward_utc_day_gem_type(ctx, utc_day)? as u8;
    let (recipients, planned_promis_load_amount) =
        validator_reward_shares(validator_topup_amount, voters)?;
    let recipient_count = reward_gem_recipient_count(&recipients)?;
    let digest = reward_gem_batch_digest(&RewardGemBatchTerms {
        reward_utc_day: utc_day,
        gem_type,
        issuance_currency: REWARD_GEM_CURRENCY,
        reference_currency: REWARD_GEM_CURRENCY,
        planned_promis_load_amount,
        recipients: &recipients,
    });
    let summary = PreparedRewardGemBatch {
        reward_utc_day: utc_day,
        planned_promis_load_amount,
        recipient_count,
        digest,
    };

    if rewards.daily_topup_prepared.read(&utc_day)? {
        require_identical_reward_gem_batch_replay(&rewards, &summary, gem_type, &recipients)?;
        return Ok(RewardGemPreparationOutcome::AlreadyPrepared(summary));
    }
    require_unprepared_day_unsettled(&rewards, utc_day)?;

    write_reward_gem_batch_terms(&rewards, &summary, gem_type)?;
    if recipients.is_empty() {
        rewards.daily_topup_prepared.write(&utc_day, true)?;
        rewards.daily_topup_settled.write(&utc_day, true)?;
        return Ok(RewardGemPreparationOutcome::NoPayableShares(summary));
    }

    require_reward_gem_currency(ctx, REWARD_GEM_CURRENCY)?;
    enqueue_reward_gem_batch(&rewards, &summary, &recipients)?;
    rewards.daily_topup_prepared.write(&utc_day, true)?;
    Ok(RewardGemPreparationOutcome::Prepared(summary))
}

fn reward_utc_day_gem_type(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<GemTypes> {
    // The type is frozen into the batch digest, so it reads the rewarded day's
    // close, never the block that happens to prepare or retry the batch.
    let day_close = date_key_to_utc_timestamp(next_date_key(utc_day));
    let gem_type = match outbe_metadosis::api::bootstrap_end_time(ctx.storage.clone())? {
        Some(end) if day_close > end => GemTypes::Validator,
        _ => GemTypes::Genesis,
    };
    Ok(gem_type)
}

fn validator_reward_shares(
    validator_topup_amount: U256,
    voters: &[(Address, u64)],
) -> Result<(Vec<(Address, U256)>, U256)> {
    let total_count = voters.iter().try_fold(0u64, |total, (_, count)| {
        total.checked_add(*count).ok_or_else(|| {
            reward_gem_retryable_error("validator reward participation total overflow")
        })
    })?;
    let mut recipients = Vec::new();
    let mut planned_promis_load_amount = U256::ZERO;
    if validator_topup_amount.is_zero() || total_count == 0 {
        return Ok((recipients, planned_promis_load_amount));
    }
    let denominator = U256::from(total_count);
    for (owner, count) in voters {
        let load = validator_reward_share(validator_topup_amount, *count, denominator)?;
        if load.is_zero() {
            continue;
        }
        if owner.is_zero() {
            return Err(reward_gem_retryable_error(
                "validator reward Gem owner is zero",
            ));
        }
        planned_promis_load_amount = planned_promis_load_amount
            .checked_add(load)
            .ok_or_else(|| reward_gem_retryable_error("validator reward planned total overflow"))?;
        recipients.push((*owner, load));
    }
    Ok((recipients, planned_promis_load_amount))
}

fn validator_reward_share(
    validator_topup_amount: U256,
    count: u64,
    denominator: U256,
) -> Result<U256> {
    if count == 0 {
        return Ok(U256::ZERO);
    }
    let weighted = validator_topup_amount
        .checked_mul(U256::from(count))
        .ok_or_else(|| reward_gem_retryable_error("validator reward share multiply overflow"))?;
    Ok(weighted / denominator)
}

fn reward_gem_recipient_count(recipients: &[(Address, U256)]) -> Result<u32> {
    if recipients.len() > outbe_consensus::bls::MAX_VALIDATORS as usize {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem batch exceeds validator bound: count={} max={}",
            recipients.len(),
            outbe_consensus::bls::MAX_VALIDATORS
        )));
    }
    u32::try_from(recipients.len())
        .map_err(|_| reward_gem_retryable_error("validator reward Gem recipient count overflow"))
}

fn require_unprepared_day_unsettled(rewards: &Rewards<'_>, utc_day: u32) -> Result<()> {
    if rewards.daily_topup_settled.read(&utc_day)? {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {utc_day} is settled without a preparation"
        )));
    }
    Ok(())
}

fn write_reward_gem_batch_terms(
    rewards: &Rewards<'_>,
    summary: &PreparedRewardGemBatch,
    gem_type: u8,
) -> Result<()> {
    let utc_day = summary.reward_utc_day;
    rewards
        .reward_gem_batch_digest
        .write(&utc_day, summary.digest)?;
    rewards
        .reward_gem_planned_load_amount
        .write(&utc_day, summary.planned_promis_load_amount)?;
    rewards.reward_gem_type.write(&utc_day, gem_type)?;
    rewards
        .reward_gem_issuance_currency
        .write(&utc_day, REWARD_GEM_CURRENCY)?;
    rewards
        .reward_gem_reference_currency
        .write(&utc_day, REWARD_GEM_CURRENCY)
}

fn enqueue_reward_gem_batch(
    rewards: &Rewards<'_>,
    summary: &PreparedRewardGemBatch,
    recipients: &[(Address, U256)],
) -> Result<()> {
    let utc_day = summary.reward_utc_day;
    let head = rewards.reward_gem_queue_head.read()?;
    let tail = rewards.reward_gem_queue_tail.read()?;
    let pending_batch_count = require_reward_gem_queue_consistency(rewards, head, tail)?;
    let next_tail = tail
        .checked_add(1)
        .ok_or_else(|| reward_gem_retryable_error("validator reward Gem FIFO sequence overflow"))?;
    let next_pending_batch_count = pending_batch_count.checked_add(1).ok_or_else(|| {
        reward_gem_retryable_error("validator reward Gem pending batch count overflow")
    })?;
    if rewards.reward_gem_queue_sequence_plus_one.read(&utc_day)? != 0 {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {utc_day} is already present in the FIFO"
        )));
    }

    let owners = rewards.reward_gem_owner_at.get_nested(&utc_day);
    let loads = rewards.reward_promis_load_at.get_nested(&utc_day);
    for (index, (owner, load)) in recipients.iter().copied().enumerate() {
        let index = u32::try_from(index).map_err(|_| {
            reward_gem_retryable_error("validator reward Gem recipient index overflow")
        })?;
        owners.write(&index, owner)?;
        loads.write(&index, load)?;
    }
    rewards
        .reward_gem_recipient_count
        .write(&utc_day, summary.recipient_count)?;
    rewards
        .reward_gem_utc_day_by_sequence
        .write(&tail, utc_day)?;
    rewards
        .reward_gem_queue_sequence_plus_one
        .write(&utc_day, next_tail)?;
    rewards.reward_gem_queue_tail.write(next_tail)?;
    rewards
        .reward_gem_pending_batch_count
        .write(next_pending_batch_count)
}
