use crate::constants::ACCRUAL_RECIPIENTS_PER_BLOCK;
use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AccrualJob, AgentRewardContract, RewardPool};
use crate::state::{enqueue_cleanup, release_pending};
use alloy_primitives::{Address, U256, U512};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

/// Performs at most 256 accrual visits and four job headers, then independent cleanup
/// passes of at most 256 entries and four headers per owner. All writes, including the
/// successful-height guard, roll back together on failure; block zero is supported.
pub fn process_agent_rewards(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.with_checkpoint(|| {
        let mut contract = ctx.contract::<AgentRewardContract>();
        if contract.worker_initialized.read()?
            && contract.worker_height.read()? == ctx.block.block_number
        {
            return Ok(());
        }
        let mut budget = ACCRUAL_RECIPIENTS_PER_BLOCK;
        for _ in 0..4 {
            let Some((mut job, pool, pending)) = next_job(&contract)? else {
                break;
            };
            credit_job_entries(ctx, &mut contract, &mut job, pool, &mut budget)?;
            if job.next_index != job.recipient_count {
                contract.accrual_jobs.update(&job)?;
                break;
            }
            finish_job(ctx, &contract, &job, pool, pending)?;
        }
        crate::api::cleanup(ctx)?;
        outbe_rewards::api::cleanup(ctx)?;
        outbe_ccaregistry::api::cleanup(ctx)?;
        contract.worker_height.write(ctx.block.block_number)?;
        contract.worker_initialized.write(true)
    })
}

fn next_job(contract: &AgentRewardContract<'_>) -> Result<Option<(AccrualJob, RewardPool, u8)>> {
    let head = contract.accrual_head.read()?;
    let tail = contract.accrual_tail.read()?;
    if head > tail || tail % 4 != 0 {
        return Err(AgentRewardError::InvalidJob.into());
    }
    if head == tail {
        return Ok(None);
    }
    let job = contract
        .accrual_jobs
        .get(head)?
        .ok_or(AgentRewardError::AccrualJobMissing(head))?;
    let pool = RewardPool::from_abi(job.pool)?;
    let expected_pool = [
        RewardPool::Validator,
        RewardPool::Waa,
        RewardPool::Sra,
        RewardPool::Cca,
    ][(head % 4) as usize];
    let pending = contract.pending_jobs_by_day.read(&job.day)?;
    // Four descriptors are published together; head % 4 is at most three and fits u8.
    let expected_pending = 4 - (head % 4) as u8;
    if job.next_index > job.recipient_count || job.credited_minor > job.pool_minor {
        return Err(AgentRewardError::InvalidJob.into());
    }
    if pool != expected_pool
        || job.day == 0
        || job.day > outbe_rewards::api::get_last_sealed_day(&contract.storage)?
        || pending != expected_pending
    {
        return Err(AgentRewardError::InvalidJob.into());
    }
    if job.total_weight.is_zero() && !job.pool_minor.is_zero() {
        return Err(AgentRewardError::InvalidJob.into());
    }
    Ok(Some((job, pool, pending)))
}

fn credit_job_entries(
    ctx: &BlockRuntimeContext,
    contract: &mut AgentRewardContract<'_>,
    job: &mut AccrualJob,
    pool: RewardPool,
    budget: &mut u32,
) -> Result<()> {
    let before = job.credited_minor;
    while *budget > 0 && job.next_index < job.recipient_count {
        let (address, weight) = source_entry(ctx, contract, job, pool)?;
        if weight > job.total_weight {
            return Err(AgentRewardError::InvalidJob.into());
        }
        let share = if job.pool_minor.is_zero() || weight.is_zero() {
            U256::ZERO
        } else {
            mul_div(job.pool_minor, weight, job.total_weight)?
        };
        job.credited_minor = job
            .credited_minor
            .checked_add(share)
            .filter(|value| *value <= job.pool_minor)
            .ok_or(AgentRewardError::InvalidJob)?;
        if !share.is_zero() {
            contract.add_claimable_reward(pool, address, share)?;
            contract.emit(IAgentReward::RewardAccrued {
                account: address,
                pool: job.pool,
                utcDay: job.day,
                promisLoadMinor: share,
            })?;
        }
        // next_index is strictly below the snapshotted u32 count.
        job.next_index += 1;
        *budget -= 1;
    }
    release_pending(contract, pool, job.credited_minor - before)
}

fn finish_job(
    ctx: &BlockRuntimeContext,
    contract: &AgentRewardContract<'_>,
    job: &AccrualJob,
    pool: RewardPool,
    pending: u8,
) -> Result<()> {
    let residue = job
        .pool_minor
        .checked_sub(job.credited_minor)
        .ok_or(AgentRewardError::InvalidJob)?;
    let emission_pool = match pool {
        RewardPool::Validator => outbe_emission::api::RewardPool::Validator,
        RewardPool::Waa => outbe_emission::api::RewardPool::Waa,
        RewardPool::Sra => outbe_emission::api::RewardPool::Sra,
        RewardPool::Cca => outbe_emission::api::RewardPool::Cca,
    };
    let record = outbe_emission::api::record_reward_pool_residue(
        ctx.storage.clone(),
        job.day,
        emission_pool,
        residue,
    )?;
    outbe_promislimit::PromisLimitContract::new(ctx.storage.clone()).checked_credit(residue)?;
    release_pending(contract, pool, residue)?;
    contract.pending_jobs_by_day.write(&job.day, pending - 1)?;
    match pool {
        RewardPool::Validator => outbe_rewards::api::enqueue_day_cleanup(ctx, job.day)?,
        RewardPool::Cca => outbe_ccaregistry::api::enqueue_day_cleanup(ctx, job.day)?,
        RewardPool::Waa | RewardPool::Sra => enqueue_cleanup(
            contract,
            WorldwideDay::new(job.day),
            pool,
            job.recipient_count,
        )?,
    }
    contract.accrual_jobs.delete(job.seq)?;
    contract
        .accrual_head
        .write(job.seq.checked_add(1).ok_or(AgentRewardError::InvalidJob)?)?;
    if pending == 1 && record.metadosis_state != outbe_emission::schema::metadosis_state::PENDING {
        outbe_emission::api::complete_day_emission_limit(ctx.storage.clone(), job.day)?;
    }
    Ok(())
}

fn source_entry(
    ctx: &BlockRuntimeContext,
    contract: &AgentRewardContract<'_>,
    job: &AccrualJob,
    pool: RewardPool,
) -> Result<(Address, U256)> {
    match pool {
        RewardPool::Validator => outbe_rewards::api::reward_voter_at(ctx, job.day, job.next_index),
        RewardPool::Cca => {
            outbe_ccaregistry::api::reward_day_entry(&ctx.storage, job.day, job.next_index)
        }
        RewardPool::Waa | RewardPool::Sra => {
            let day = WorldwideDay::new(job.day);
            let index = contract.day_index(pool)?;
            let address = index
                .addresses
                .read(&AgentRewardContract::address_index_key(day, job.next_index))?;
            let count_key = AgentRewardContract::tribute_count_key(day, address);
            let count = index.counts.read(&count_key)?;
            // WAA/SRA append only on a positive first contribution and remain immutable.
            // Address zero itself is valid; a missing positive count is not.
            if count == 0
                || index.member_index.read(&count_key)?
                    != job
                        .next_index
                        .checked_add(1)
                        .ok_or(AgentRewardError::InvalidJob)?
            {
                return Err(AgentRewardError::InvalidJob.into());
            }
            Ok((address, U256::from(count)))
        }
    }
}

fn mul_div(a: U256, b: U256, denominator: U256) -> Result<U256> {
    // A product of two U256 values fits U512 without truncation.
    let value = (U512::from(a) * U512::from(b))
        .checked_div(U512::from(denominator))
        .ok_or(AgentRewardError::DistributionOverflow)?;
    if value > U512::from(U256::MAX) {
        return Err(AgentRewardError::DistributionOverflow.into());
    }
    // The range check proves the narrowing conversion is lossless.
    Ok(value.wrapping_to::<U256>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn full_width_pool_products_floor_without_intermediate_overflow(
            pool in any::<[u64; 4]>().prop_map(U256::from_limbs),
            weight in any::<[u64; 4]>().prop_map(U256::from_limbs),
            other in any::<[u64; 4]>().prop_map(U256::from_limbs),
        ) {
            let total = weight.max(other).max(U256::ONE);
            let share = mul_div(pool, weight, total).unwrap();
            let product = U512::from(pool) * U512::from(weight);
            prop_assert!(U512::from(share) * U512::from(total) <= product);
            prop_assert!(product < (U512::from(share) + U512::ONE) * U512::from(total));
            prop_assert!(share <= pool);
        }
    }

    #[test]
    fn wide_product_narrowing_and_zero_denominator_are_checked() {
        assert_eq!(mul_div(U256::MAX, U256::MAX, U256::MAX).unwrap(), U256::MAX);
        assert!(mul_div(U256::MAX, U256::MAX, U256::ONE).is_err());
        assert!(mul_div(U256::ONE, U256::ONE, U256::ZERO).is_err());
    }
}
