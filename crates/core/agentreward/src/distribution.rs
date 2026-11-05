use crate::errors::AgentRewardError;
use crate::schema::{AccrualJob, AgentRewardContract, RewardPool};
use crate::state::{enqueue_cleanup, prepare_job};
use alloy_primitives::U256;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

mod worker;

pub use worker::process_agent_rewards;

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
