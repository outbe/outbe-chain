use alloy_primitives::{Address, U256};
use outbe_gemfactory::GemTypes;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use super::batch::{
    require_reward_gem_currency, require_reward_gem_queue_consistency, reward_gem_batch_digest,
    reward_gem_retryable_error, RewardGemBatchTerms,
};
use super::RewardGemDeliveryOutcome;
use crate::constants::REWARD_GEM_CURRENCY;
use crate::schema::Rewards;

struct RewardGemFifoHead {
    sequence: u64,
    next_sequence: u64,
    pending_batch_count: u64,
    reward_utc_day: u32,
}

struct QueuedRewardGemBatch {
    gem_type: GemTypes,
    issuance_currency: u16,
    reference_currency: u16,
    recipients: Vec<(Address, U256)>,
    promis_load_amount: U256,
}

/// Attempts to deliver exactly one complete FIFO head batch. Missing or stale
/// price data is a successful no-op. A mint failure leaves atomic rollback to
/// this checkpoint and to the enclosing system transaction.
pub fn deliver_oldest_reward_gem_batch(
    ctx: &BlockRuntimeContext,
) -> Result<RewardGemDeliveryOutcome> {
    ctx.with_checkpoint(|| deliver_oldest_reward_gem_batch_inner(ctx))
}

fn deliver_oldest_reward_gem_batch_inner(
    ctx: &BlockRuntimeContext,
) -> Result<RewardGemDeliveryOutcome> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let Some(head) = read_reward_gem_fifo_head(&rewards)? else {
        return Ok(RewardGemDeliveryOutcome::Empty);
    };
    let reward_utc_day = head.reward_utc_day;
    let recipient_count = read_reward_gem_recipient_count(&rewards, reward_utc_day)?;
    let batch = read_queued_reward_gem_batch(&rewards, reward_utc_day, recipient_count)?;

    require_reward_gem_currency(ctx, batch.reference_currency)?;
    let Some(entry_price) = resolve_reward_entry_price(ctx, batch.reference_currency)? else {
        tracing::warn!(
            target: "outbe::rewards",
            reward_utc_day,
            "validator reward Gem batch has no usable price yet, staying queued"
        );
        return Ok(RewardGemDeliveryOutcome::PendingRate { reward_utc_day });
    };

    let delivered_promis_load_amount = batch.promis_load_amount;
    issue_reward_gems(ctx, batch, entry_price)?;
    retire_reward_gem_fifo_head(&rewards, &head, recipient_count)?;

    Ok(RewardGemDeliveryOutcome::Delivered {
        reward_utc_day,
        recipient_count,
        delivered_promis_load_amount,
    })
}

fn read_reward_gem_fifo_head(rewards: &Rewards<'_>) -> Result<Option<RewardGemFifoHead>> {
    let head = rewards.reward_gem_queue_head.read()?;
    let tail = rewards.reward_gem_queue_tail.read()?;
    let pending_batch_count = require_reward_gem_queue_consistency(rewards, head, tail)?;
    if head == tail {
        require_empty_reward_gem_fifo(rewards, head)?;
        return Ok(None);
    }

    let reward_utc_day = rewards.reward_gem_utc_day_by_sequence.read(&head)?;
    if reward_utc_day == 0 {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem FIFO head {head} has no reward UTC day"
        )));
    }
    let next_sequence = head
        .checked_add(1)
        .ok_or_else(|| reward_gem_retryable_error("validator reward Gem FIFO sequence overflow"))?;
    require_reward_gem_fifo_head_state(rewards, reward_utc_day, next_sequence)?;
    Ok(Some(RewardGemFifoHead {
        sequence: head,
        next_sequence,
        pending_batch_count,
        reward_utc_day,
    }))
}

fn require_empty_reward_gem_fifo(rewards: &Rewards<'_>, head: u64) -> Result<()> {
    if rewards.reward_gem_utc_day_by_sequence.read(&head)? != 0 {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem empty FIFO has a live UTC day at sequence {head}"
        )));
    }
    Ok(())
}

fn require_reward_gem_fifo_head_state(
    rewards: &Rewards<'_>,
    reward_utc_day: u32,
    next_sequence: u64,
) -> Result<()> {
    if rewards
        .reward_gem_queue_sequence_plus_one
        .read(&reward_utc_day)?
        != next_sequence
    {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem FIFO reverse index disagrees for UTC day {reward_utc_day}"
        )));
    }
    if !rewards.daily_topup_prepared.read(&reward_utc_day)?
        || rewards.daily_topup_settled.read(&reward_utc_day)?
    {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem FIFO UTC day {reward_utc_day} has an illegal state"
        )));
    }
    Ok(())
}

fn read_reward_gem_recipient_count(rewards: &Rewards<'_>, reward_utc_day: u32) -> Result<u32> {
    let recipient_count = rewards.reward_gem_recipient_count.read(&reward_utc_day)?;
    if recipient_count == 0 || recipient_count > outbe_consensus::bls::MAX_VALIDATORS {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {reward_utc_day} has invalid recipient count {recipient_count}"
        )));
    }
    Ok(recipient_count)
}

fn read_queued_reward_gem_batch(
    rewards: &Rewards<'_>,
    reward_utc_day: u32,
    recipient_count: u32,
) -> Result<QueuedRewardGemBatch> {
    let gem_type_raw = rewards.reward_gem_type.read(&reward_utc_day)?;
    let gem_type = reward_gem_type_from_raw(reward_utc_day, gem_type_raw)?;
    let (issuance_currency, reference_currency) =
        read_reward_gem_currencies(rewards, reward_utc_day)?;
    let (recipients, promis_load_amount) =
        read_reward_gem_recipients(rewards, reward_utc_day, recipient_count)?;
    if promis_load_amount
        != rewards
            .reward_gem_planned_load_amount
            .read(&reward_utc_day)?
    {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {reward_utc_day} total disagrees with its preparation"
        )));
    }
    let digest = reward_gem_batch_digest(&RewardGemBatchTerms {
        reward_utc_day,
        gem_type: gem_type_raw,
        issuance_currency,
        reference_currency,
        planned_promis_load_amount: promis_load_amount,
        recipients: &recipients,
    });
    if digest != rewards.reward_gem_batch_digest.read(&reward_utc_day)? {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {reward_utc_day} digest disagrees with its preparation"
        )));
    }
    Ok(QueuedRewardGemBatch {
        gem_type,
        issuance_currency,
        reference_currency,
        recipients,
        promis_load_amount,
    })
}

fn reward_gem_type_from_raw(reward_utc_day: u32, gem_type_raw: u8) -> Result<GemTypes> {
    match gem_type_raw {
        value if value == GemTypes::Genesis as u8 => Ok(GemTypes::Genesis),
        value if value == GemTypes::Validator as u8 => Ok(GemTypes::Validator),
        _ => Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {reward_utc_day} has unsupported type {gem_type_raw}"
        ))),
    }
}

fn read_reward_gem_currencies(rewards: &Rewards<'_>, reward_utc_day: u32) -> Result<(u16, u16)> {
    let issuance_currency = rewards.reward_gem_issuance_currency.read(&reward_utc_day)?;
    let reference_currency = rewards
        .reward_gem_reference_currency
        .read(&reward_utc_day)?;
    if issuance_currency != REWARD_GEM_CURRENCY || reference_currency != REWARD_GEM_CURRENCY {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem UTC day {reward_utc_day} has invalid currencies {issuance_currency}/{reference_currency}"
        )));
    }
    Ok((issuance_currency, reference_currency))
}

fn read_reward_gem_recipients(
    rewards: &Rewards<'_>,
    reward_utc_day: u32,
    recipient_count: u32,
) -> Result<(Vec<(Address, U256)>, U256)> {
    let owners = rewards.reward_gem_owner_at.get_nested(&reward_utc_day);
    let loads = rewards.reward_promis_load_at.get_nested(&reward_utc_day);
    let mut recipients = Vec::with_capacity(recipient_count as usize);
    let mut promis_load_amount = U256::ZERO;
    for index in 0..recipient_count {
        let owner = owners.read(&index)?;
        let load = loads.read(&index)?;
        if owner.is_zero() || load.is_zero() {
            return Err(reward_gem_retryable_error(format!(
                "validator reward Gem UTC day {reward_utc_day} has an empty recipient at index {index}"
            )));
        }
        promis_load_amount = promis_load_amount.checked_add(load).ok_or_else(|| {
            reward_gem_retryable_error("validator reward Gem delivery total overflow")
        })?;
        recipients.push((owner, load));
    }
    Ok((recipients, promis_load_amount))
}

fn resolve_reward_entry_price(
    ctx: &BlockRuntimeContext,
    reference_currency: u16,
) -> Result<Option<U256>> {
    let day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
    );
    outbe_oracle::api::get_utc_day_vwap_for_iso(ctx.storage.clone(), day, reference_currency)
}

fn issue_reward_gems(
    ctx: &BlockRuntimeContext,
    batch: QueuedRewardGemBatch,
    entry_price: U256,
) -> Result<()> {
    for (owner, load) in batch.recipients {
        outbe_gemfactory::api::issue_gem(
            &ctx.storage,
            outbe_gemfactory::GemIssueParams {
                owner,
                gem_type: batch.gem_type,
                promis_load: load,
                issuance_currency: batch.issuance_currency,
                reference_currency: batch.reference_currency,
                entry_price,
            },
        )?;
    }
    Ok(())
}

fn retire_reward_gem_fifo_head(
    rewards: &Rewards<'_>,
    head: &RewardGemFifoHead,
    recipient_count: u32,
) -> Result<()> {
    let reward_utc_day = head.reward_utc_day;
    let owners = rewards.reward_gem_owner_at.get_nested(&reward_utc_day);
    let loads = rewards.reward_promis_load_at.get_nested(&reward_utc_day);
    for index in 0..recipient_count {
        owners.write(&index, Address::ZERO)?;
        loads.write(&index, U256::ZERO)?;
    }
    rewards
        .reward_gem_recipient_count
        .write(&reward_utc_day, 0)?;
    rewards
        .reward_gem_utc_day_by_sequence
        .write(&head.sequence, 0)?;
    rewards
        .reward_gem_queue_sequence_plus_one
        .write(&reward_utc_day, 0)?;
    rewards.daily_topup_settled.write(&reward_utc_day, true)?;
    rewards.reward_gem_queue_head.write(head.next_sequence)?;
    rewards.reward_gem_pending_batch_count.write(
        head.pending_batch_count.checked_sub(1).ok_or_else(|| {
            reward_gem_retryable_error("validator reward Gem pending batch count underflow")
        })?,
    )
}
