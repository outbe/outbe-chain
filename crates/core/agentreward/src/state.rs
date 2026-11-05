//! Local pending accounting and bounded disposal of settled day inputs.

use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, time::WorldwideDay};

use crate::{
    constants::ACCRUAL_RECIPIENTS_PER_BLOCK,
    errors::AgentRewardError,
    schema::{AccrualJob, AgentRewardContract, DayIndex, RewardPool},
};

/// Prevalidates a descriptor and returns its no-weight excess and new pending total.
pub(crate) fn prepare_job(
    contract: &AgentRewardContract<'_>,
    job: &mut AccrualJob,
) -> Result<(U256, U256)> {
    if job.recipient_count == 0 && !job.total_weight.is_zero() {
        return Err(AgentRewardError::InvalidJob.into());
    }
    let nominal = job.pool_minor;
    if job.total_weight.is_zero() {
        job.pool_minor = U256::ZERO;
    }
    let pending = contract
        .pending_total_minor
        .read(&job.pool)?
        .checked_add(job.pool_minor)
        .ok_or(AgentRewardError::DistributionOverflow)?;
    if contract.accrual_jobs.get(job.seq)?.is_some() {
        return Err(AgentRewardError::InvalidJob.into());
    }
    Ok((nominal - job.pool_minor, pending))
}

pub(crate) fn release_pending(
    contract: &AgentRewardContract<'_>,
    pool: RewardPool,
    amount: U256,
) -> Result<()> {
    let pending = contract
        .pending_total_minor
        .read(&(pool as u8))?
        .checked_sub(amount)
        .ok_or(AgentRewardError::InvalidJob)?;
    contract.pending_total_minor.write(&(pool as u8), pending)
}

pub(crate) fn enqueue_cleanup(
    contract: &AgentRewardContract<'_>,
    day: WorldwideDay,
    pool: RewardPool,
    count: u32,
) -> Result<()> {
    let key = AgentRewardContract::address_index_key(day, u32::from(pool as u8));
    if contract.cleanup_scheduled.read(&key)? {
        return Ok(());
    }
    let seq = contract.cleanup_tail.read()?;
    let tail = seq.checked_add(1).ok_or(AgentRewardError::InvalidJob)?;
    contract.cleanup_jobs.create(&AccrualJob {
        seq,
        day: day.value(),
        pool: pool as u8,
        pool_minor: U256::ZERO,
        next_index: 0,
        credited_minor: U256::ZERO,
        recipient_count: count,
        total_weight: U256::ZERO,
    })?;
    contract.cleanup_tail.write(tail)?;
    contract.cleanup_scheduled.write(&key, true)
}

pub(crate) fn cleanup(contract: &AgentRewardContract<'_>) -> Result<()> {
    let mut budget = ACCRUAL_RECIPIENTS_PER_BLOCK;
    for _ in 0..4 {
        let head = contract.cleanup_head.read()?;
        let tail = contract.cleanup_tail.read()?;
        if head > tail {
            return Err(AgentRewardError::InvalidJob.into());
        }
        if head == tail {
            break;
        }
        let mut job = contract
            .cleanup_jobs
            .get(head)?
            .ok_or(AgentRewardError::InvalidJob)?;
        let pool = RewardPool::from_abi(job.pool)?;
        let day = WorldwideDay::new(job.day);
        if !(1..=outbe_rewards::api::get_last_sealed_day(&contract.storage)?).contains(&job.day) {
            return Err(AgentRewardError::InvalidJob.into());
        }
        let index = contract.day_index(pool)?;
        if job.next_index > job.recipient_count {
            return Err(AgentRewardError::InvalidJob.into());
        }
        cleanup_entries(&index, day, &mut job, &mut budget)?;
        if job.next_index != job.recipient_count {
            contract.cleanup_jobs.update(&job)?;
            break;
        }
        index.total.write(&day, 0)?;
        index.address_count.write(&day, 0)?;
        contract.cleanup_jobs.delete(head)?;
        contract
            .cleanup_head
            .write(head.checked_add(1).ok_or(AgentRewardError::InvalidJob)?)?;
    }
    Ok(())
}

fn cleanup_entries(
    index: &DayIndex<'_, '_>,
    day: WorldwideDay,
    job: &mut AccrualJob,
    budget: &mut u32,
) -> Result<()> {
    while *budget > 0 && job.next_index < job.recipient_count {
        let key = AgentRewardContract::address_index_key(day, job.next_index);
        let address = index.addresses.read(&key)?;
        let count_key = AgentRewardContract::tribute_count_key(day, address);
        if index.member_index.read(&count_key)?
            != job
                .next_index
                .checked_add(1)
                .ok_or(AgentRewardError::InvalidJob)?
        {
            return Err(AgentRewardError::InvalidJob.into());
        }
        index.counts.write(&count_key, 0)?;
        index.member_index.write(&count_key, 0)?;
        index.addresses.write(&key, Address::ZERO)?;
        job.next_index += 1;
        *budget -= 1;
    }
    Ok(())
}
