use crate::constants::ACCRUAL_RECIPIENTS_PER_BLOCK;
use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AccrualJob, AgentRewardContract, RewardPool};
use crate::state::{enqueue_cleanup, prepare_job, release_pending};
use alloy_primitives::{Address, U256, U512};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayAgentAllocation {
    pub validators_allocated: U256,
    pub waa_allocated: U256,
    pub sra_allocated: U256,
    pub cca_allocated: U256,
    pub total_excess: U256,
}

/// Nominal protocol-unit amounts before no-weight excess is returned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayAgentPools {
    pub validator_topup: U256,
    pub waa: U256,
    pub sra: U256,
    pub cca: U256,
}

/// Captures immutable day inputs and queues exactly four jobs. No recipient is credited here.
pub fn allocate_day_agent_rewards(
    ctx: &BlockRuntimeContext,
    day: WorldwideDay,
    pools: DayAgentPools,
) -> Result<DayAgentAllocation> {
    ctx.storage.with_checkpoint(|| {
        let contract = ctx.contract::<AgentRewardContract>();
        if day.value() == 0 || day.value() != outbe_rewards::api::get_last_sealed_day(&ctx.storage)?
        {
            return Err(AgentRewardError::InvalidJob.into());
        }
        for pool in [RewardPool::Waa, RewardPool::Sra] {
            let key = AgentRewardContract::address_index_key(day, u32::from(pool as u8));
            if contract.cleanup_scheduled.read(&key)? {
                return Err(AgentRewardError::DaySealed(day.value()).into());
            }
        }
        let head = contract.accrual_head.read()?;
        let tail = contract.accrual_tail.read()?;
        let next_tail = tail.checked_add(4).ok_or(AgentRewardError::InvalidJob)?;
        if head > tail || tail % 4 != 0 || contract.pending_jobs_by_day.read(&day.value())? != 0 {
            return Err(AgentRewardError::InvalidJob.into());
        }
        let validator_inputs = outbe_rewards::api::sealed_day_inputs(ctx, day.value())?;
        let cca_inputs = outbe_ccaregistry::api::reward_day_inputs(&ctx.storage, day.value())?;
        let waa_inputs = (
            contract.waa_address_count.read(&day)?,
            U256::from(contract.waa_total_count.read(&day)?),
        );
        let sra_inputs = (
            contract.sra_address_count.read(&day)?,
            U256::from(contract.sra_total_count.read(&day)?),
        );
        let descriptors = [
            (
                RewardPool::Validator,
                pools.validator_topup,
                validator_inputs,
            ),
            (RewardPool::Waa, pools.waa, waa_inputs),
            (RewardPool::Sra, pools.sra, sra_inputs),
            (RewardPool::Cca, pools.cca, cca_inputs),
        ];
        let mut jobs = Vec::with_capacity(4);
        let mut excess = U256::ZERO;
        let mut assigned = [U256::ZERO; 4];
        for (offset, (pool, nominal, (count, weight))) in descriptors.into_iter().enumerate() {
            // There are exactly four descriptors, so the offset fits u64.
            let seq = tail + offset as u64;
            let mut job = AccrualJob {
                seq,
                day: day.value(),
                pool: pool as u8,
                pool_minor: nominal,
                next_index: 0,
                credited_minor: U256::ZERO,
                recipient_count: count,
                total_weight: weight,
            };
            let (pool_excess, pending) = prepare_job(&contract, &mut job)?;
            excess = excess
                .checked_add(pool_excess)
                .ok_or(AgentRewardError::DistributionOverflow)?;
            assigned[offset] = job.pool_minor;
            jobs.push((job, pending));
        }
        // Validate aggregate arithmetic before publishing any descriptor.
        assigned.iter().try_fold(excess, |sum, amount| {
            sum.checked_add(*amount)
                .ok_or(AgentRewardError::DistributionOverflow)
        })?;
        for (job, pending) in jobs {
            contract.accrual_jobs.create(&job)?;
            contract.pending_total_minor.write(&job.pool, pending)?;
        }
        contract.accrual_tail.write(next_tail)?;
        contract.pending_jobs_by_day.write(&day.value(), 4)?;
        Ok(DayAgentAllocation {
            validators_allocated: assigned[0],
            waa_allocated: assigned[1],
            sra_allocated: assigned[2],
            cca_allocated: assigned[3],
            total_excess: excess,
        })
    })
}

/// Constant-time monetary-pending lookup, including zero-value jobs awaiting finalization.
pub fn has_pending_accruals_for_day(storage: &StorageHandle<'_>, day: u32) -> Result<bool> {
    let count = AgentRewardContract::new(storage.clone())
        .pending_jobs_by_day
        .read(&day)?;
    if count > 4 {
        return Err(AgentRewardError::InvalidJob.into());
    }
    Ok(count != 0)
}

/// Abandons sealed inputs without creating monetary obligations. Scheduling is idempotent.
pub fn abandon_day_agent_rewards(ctx: &BlockRuntimeContext, day: WorldwideDay) -> Result<()> {
    ctx.storage.with_checkpoint(|| {
        let contract = ctx.contract::<AgentRewardContract>();
        if has_pending_accruals_for_day(&ctx.storage, day.value())? {
            return Err(AgentRewardError::InvalidJob.into());
        }
        if day.value() == 0 || day.value() > outbe_rewards::api::get_last_sealed_day(&ctx.storage)?
        {
            return Err(AgentRewardError::InvalidJob.into());
        }
        outbe_rewards::api::enqueue_day_cleanup(ctx, day.value())?;
        outbe_ccaregistry::api::enqueue_day_cleanup(ctx, day.value())?;
        for pool in [RewardPool::Waa, RewardPool::Sra] {
            let count = contract.day_index(pool)?.address_count.read(&day)?;
            enqueue_cleanup(&contract, day, pool, count)?;
        }
        Ok(())
    })
}

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
        outbe_emission::api::complete_emission_day(ctx.storage.clone(), job.day)?;
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
