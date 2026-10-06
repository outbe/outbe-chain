use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

use crate::schema::Rewards;

const REWARD_GEM_BATCH_DIGEST_DOMAIN: &[u8] = b"OUTBE_REWARD_GEM_BATCH_V1";

pub(super) struct RewardGemBatchTerms<'a> {
    pub(super) reward_utc_day: u32,
    pub(super) gem_type: u8,
    pub(super) issuance_currency: u16,
    pub(super) reference_currency: u16,
    pub(super) planned_promis_load_amount: U256,
    pub(super) recipients: &'a [(Address, U256)],
}

pub(super) fn reward_gem_retryable_error(message: impl Into<String>) -> PrecompileError {
    let message = message.into();
    tracing::error!(
        target: "outbe::rewards",
        error = %message,
        "validator reward Gem batch remains pending"
    );
    PrecompileError::Revert(message)
}

pub(super) fn require_reward_gem_queue_consistency(
    rewards: &Rewards<'_>,
    head: u64,
    tail: u64,
) -> Result<u64> {
    let span = tail
        .checked_sub(head)
        .ok_or_else(|| reward_gem_retryable_error("validator reward Gem FIFO head exceeds tail"))?;
    let pending_batch_count = rewards.reward_gem_pending_batch_count.read()?;
    if pending_batch_count != span {
        return Err(reward_gem_retryable_error(format!(
            "validator reward Gem pending batch count {pending_batch_count} disagrees with FIFO span {span}"
        )));
    }
    Ok(pending_batch_count)
}

pub(super) fn require_reward_gem_currency(ctx: &BlockRuntimeContext, currency: u16) -> Result<()> {
    outbe_oracle::api::require_coen_pair(ctx.storage.clone(), currency).map_err(|error| {
        reward_gem_retryable_error(format!(
            "validator reward Gem currency is not registered: {error}"
        ))
    })?;
    Ok(())
}

pub(super) fn reward_gem_batch_digest(terms: &RewardGemBatchTerms<'_>) -> B256 {
    let recipients = terms.recipients;
    let mut bytes = Vec::with_capacity(
        REWARD_GEM_BATCH_DIGEST_DOMAIN.len() + 4 + 1 + 2 + 2 + 4 + 32 + recipients.len() * 52,
    );
    bytes.extend_from_slice(REWARD_GEM_BATCH_DIGEST_DOMAIN);
    bytes.extend_from_slice(&terms.reward_utc_day.to_be_bytes());
    bytes.push(terms.gem_type);
    bytes.extend_from_slice(&terms.issuance_currency.to_be_bytes());
    bytes.extend_from_slice(&terms.reference_currency.to_be_bytes());
    bytes.extend_from_slice(&(recipients.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&terms.planned_promis_load_amount.to_be_bytes::<32>());
    for (owner, load) in recipients {
        bytes.extend_from_slice(owner.as_slice());
        bytes.extend_from_slice(&load.to_be_bytes::<32>());
    }
    keccak256(bytes)
}
