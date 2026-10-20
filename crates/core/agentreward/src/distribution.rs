use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AccrualJob, AgentRewardContract, RewardPool};
use alloy_primitives::{Address, U256, U512};
use outbe_primitives::error::Result as PrecompileResult;
use outbe_primitives::time::WorldwideDay;

// ------------------------------------------------------------------------
// New daily orchestrator surface
// ------------------------------------------------------------------------

/// One of the reward pools that AgentReward owns end-to-end.
///
/// The EmissionLimit daily handler splits the emission between pool kinds.
/// This enum is the protocol contract between that orchestrator and
/// AgentReward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolKind {
    /// WAA (wallet) proportional distribution pool. Uses tribute counts kept
    /// in `waa_*` storage fields.
    Waa,
    /// SRA (signer-of-record-attestation) proportional distribution pool. Uses
    /// tribute counts kept in `sra_*` storage fields.
    Sra,
    /// CCA rewards weighted by net origination recorded in the CCA registry.
    Cca,
    /// Validator top-up weighted by the day's finalized-block participation
    /// recorded in Rewards.
    Validator,
}

/// Daily orchestrator entrypoint called by the EmissionLimit Cycle
/// handler. Dispatches each `(PoolKind, amount)` pair into
/// the correct sub-routine and returns the sum of pool excesses that
/// should be added to the Metadosis terminal credit.
///
/// Excess is rounding residue, or the whole pool when no positive eligible
/// weight exists. WAA and SRA pools with tributes go to [`advance_accruals`].
/// Shares are not capped per address.
///
/// Claimable balances and the returned excess are protocol units.
pub fn distribute_daily(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> PrecompileResult<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut total_excess = U256::ZERO;
        for (kind, amount) in pools {
            let excess = distribute_pool(ctx, prev_day, *kind, *amount)?;
            total_excess = total_excess
                .checked_add(excess)
                .ok_or(AgentRewardError::DistributeDailyOverflow)?;
        }
        Ok(total_excess)
    })
}

/// Allocates one agent pool proportionally to its weights.
/// The function returns the undistributed protocol units.
fn distribute_pool(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    kind: PoolKind,
    amount: U256,
) -> PrecompileResult<U256> {
    if amount.is_zero() {
        return Ok(U256::ZERO);
    }
    let mut contract = ctx.contract::<AgentRewardContract>();
    let (reward_pool, weights) = match kind {
        PoolKind::Waa => return enqueue_day_pool(&mut contract, prev_day, RewardPool::Waa, amount),
        PoolKind::Sra => return enqueue_day_pool(&mut contract, prev_day, RewardPool::Sra, amount),
        PoolKind::Cca => (
            RewardPool::Cca,
            outbe_ccaregistry::api::active_reward_weights(&ctx.storage, prev_day.value())?,
        ),
        PoolKind::Validator => (
            RewardPool::Validator,
            outbe_rewards::api::read_voters_for_day(ctx, prev_day.value())?
                .into_iter()
                .map(|(address, count)| (address, U256::from(count)))
                .collect(),
        ),
    };
    let (rewards, excess) = calculate_proportional_distribution(amount, &weights)?;
    for reward in rewards {
        if reward.reward_amount.is_zero() {
            continue;
        }
        contract.add_claimable_reward(reward_pool, reward.address, reward.reward_amount)?;
        contract.emit(IAgentReward::RewardAccrued {
            account: reward.address,
            pool: reward_pool as u8,
            utcDay: prev_day.value(),
            promisLoadMinor: reward.reward_amount,
        })?;
    }
    Ok(excess)
}

/// Most WAA and SRA addresses that one block credits.
pub const ACCRUAL_RECIPIENTS_PER_BLOCK: u32 = 256;

/// Queues a WAA or SRA day pool for [`advance_accruals`]. A day without tributes
/// returns the whole pool as excess.
fn enqueue_day_pool(
    contract: &mut AgentRewardContract<'_>,
    day: WorldwideDay,
    pool: RewardPool,
    amount: U256,
) -> PrecompileResult<U256> {
    if contract.day_index(pool)?.total.read(&day)? == 0 {
        return Ok(amount);
    }
    let seq = contract.accrual_tail.read()?;
    contract.accrual_jobs.create(&AccrualJob {
        seq,
        day: day.value(),
        pool: pool as u8,
        pool_minor: amount,
        next_index: 0,
        credited_minor: U256::ZERO,
    })?;
    contract.accrual_tail.write(seq + 1)?;
    let pending = contract
        .pending_total_minor
        .read(&(pool as u8))?
        .checked_add(amount)
        .ok_or(AgentRewardError::DistributionOverflow)?;
    contract.pending_total_minor.write(&(pool as u8), pending)?;
    Ok(U256::ZERO)
}

/// Credits up to [`ACCRUAL_RECIPIENTS_PER_BLOCK`] queued addresses, oldest job first,
/// and returns the rounding residue of the jobs that finish.
pub fn advance_accruals(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
) -> PrecompileResult<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut contract = ctx.contract::<AgentRewardContract>();
        let mut budget = ACCRUAL_RECIPIENTS_PER_BLOCK;
        let mut residue = U256::ZERO;
        while budget > 0 {
            let head = contract.accrual_head.read()?;
            if head == contract.accrual_tail.read()? {
                break;
            }
            let mut job = contract
                .accrual_jobs
                .get(head)?
                .ok_or(AgentRewardError::AccrualJobMissing(head))?;
            let pool = RewardPool::from_abi(job.pool)?;
            let day = WorldwideDay::new(job.day);
            let index = contract.day_index(pool)?;
            let count = index.address_count.read(&day)?;
            let total_weight = U256::from(index.total.read(&day)?);
            let credited_before = job.credited_minor;
            while job.next_index < count && budget > 0 {
                let credited = credit_day_entry(&mut contract, &job, pool, day, total_weight)?;
                job.credited_minor = job
                    .credited_minor
                    .checked_add(credited)
                    .ok_or(AgentRewardError::DistributionOverflow)?;
                job.next_index += 1;
                budget -= 1;
            }
            let mut released = job.credited_minor - credited_before;
            if job.next_index < count {
                release_pending(&mut contract, pool, released)?;
                contract.accrual_jobs.update(&job)?;
                break;
            }
            let index = contract.day_index(pool)?;
            index.address_count.write(&day, 0)?;
            index.total.write(&day, 0)?;
            let job_residue = job
                .pool_minor
                .checked_sub(job.credited_minor)
                .ok_or(AgentRewardError::DistributionOverflow)?;
            residue = residue
                .checked_add(job_residue)
                .ok_or(AgentRewardError::DistributionOverflow)?;
            released += job_residue;
            release_pending(&mut contract, pool, released)?;
            contract.accrual_jobs.delete(head)?;
            contract.accrual_head.write(head + 1)?;
        }
        Ok(residue)
    })
}

fn release_pending(
    contract: &mut AgentRewardContract<'_>,
    pool: RewardPool,
    amount: U256,
) -> PrecompileResult<()> {
    let pending = contract
        .pending_total_minor
        .read(&(pool as u8))?
        .checked_sub(amount)
        .ok_or(AgentRewardError::DistributionOverflow)?;
    contract.pending_total_minor.write(&(pool as u8), pending)
}

/// Credits the address at `job.next_index` and clears its day entry.
fn credit_day_entry(
    contract: &mut AgentRewardContract<'_>,
    job: &AccrualJob,
    pool: RewardPool,
    day: WorldwideDay,
    total_weight: U256,
) -> PrecompileResult<U256> {
    let index_key = AgentRewardContract::address_index_key(day, job.next_index);
    let (address, count_key, weight) = {
        let index = contract.day_index(pool)?;
        let address = index.addresses.read(&index_key)?;
        let count_key = AgentRewardContract::tribute_count_key(day, address);
        (address, count_key, index.counts.read(&count_key)?)
    };
    let share = mul_div(job.pool_minor, U256::from(weight), total_weight)?;
    if !share.is_zero() {
        contract.add_claimable_reward(pool, address, share)?;
        contract.emit(IAgentReward::RewardAccrued {
            account: address,
            pool: pool as u8,
            utcDay: day.value(),
            promisLoadMinor: share,
        })?;
    }
    let index = contract.day_index(pool)?;
    index.counts.write(&count_key, 0)?;
    index.addresses.write(&index_key, Address::ZERO)?;
    Ok(share)
}

fn mul_div(a: U256, b: U256, denominator: U256) -> Result<U256, AgentRewardError> {
    // A product of two U256 values fits U512 without truncation.
    let value = (U512::from(a) * U512::from(b))
        .checked_div(U512::from(denominator))
        .ok_or(AgentRewardError::DistributionOverflow)?;
    if value > U512::from(U256::MAX) {
        return Err(AgentRewardError::DistributionOverflow);
    }
    // The range check above proves the narrowing conversion is lossless.
    Ok(value.wrapping_to::<U256>())
}

/// Reward allocated to a single address.
pub struct AddressReward {
    pub address: Address,
    pub weight: U256,
    pub reward_amount: U256,
}

/// Distributes a pool in proportion to weights, with no per-address cap.
///
/// Each positive weight receives `floor(pool * weight / total_weight)`.
/// The undistributed remainder is excess, including an empty or all-zero
/// weight set. Callers supply one weight per distinct address. The function
/// orders the results by address.
pub(crate) fn calculate_proportional_distribution(
    total_pool: U256,
    counts: &[(Address, U256)],
) -> Result<(Vec<AddressReward>, U256), AgentRewardError> {
    if counts.is_empty() || total_pool.is_zero() {
        return Ok((vec![], total_pool));
    }

    let mut positive: Vec<_> = counts
        .iter()
        .copied()
        .filter(|(_, weight)| !weight.is_zero())
        .collect();
    if positive.is_empty() {
        return Ok((vec![], total_pool));
    }
    positive.sort_by_key(|(addr, _)| *addr);

    let total_weight = positive.iter().try_fold(U256::ZERO, |sum, (_, weight)| {
        sum.checked_add(*weight)
            .ok_or(AgentRewardError::DistributionOverflow)
    })?;

    let mut rewards = Vec::with_capacity(positive.len());
    let mut distributed = U256::ZERO;
    for (addr, weight) in positive {
        let share = mul_div(total_pool, weight, total_weight)?;
        distributed = distributed
            .checked_add(share)
            .ok_or(AgentRewardError::DistributionOverflow)?;
        rewards.push(AddressReward {
            address: addr,
            weight,
            reward_amount: share,
        });
    }
    let excess = total_pool
        .checked_sub(distributed)
        .ok_or(AgentRewardError::DistributionOverflow)?;
    Ok((rewards, excess))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_pool_has_no_rewards_or_excess() {
        let (rewards, excess) = calculate_proportional_distribution(
            U256::ZERO,
            &[(Address::repeat_byte(1), U256::ONE)],
        )
        .unwrap();
        assert!(rewards.is_empty());
        assert_eq!(excess, U256::ZERO);
    }

    #[test]
    fn wide_products_and_checked_overflow() {
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let (rewards, excess) =
            calculate_proportional_distribution(U256::MAX, &[(a, U256::MAX)]).unwrap();
        assert_eq!(rewards[0].reward_amount, U256::MAX);
        assert_eq!(rewards[0].weight, U256::MAX);
        assert_eq!(excess, U256::ZERO);
        assert!(
            calculate_proportional_distribution(U256::ONE, &[(a, U256::MAX), (b, U256::ONE)])
                .is_err()
        );
        let (rewards, excess) = calculate_proportional_distribution(
            U256::from(1000),
            &[(a, U256::from(u64::MAX)), (b, U256::from(u64::MAX))],
        )
        .unwrap();
        assert_eq!(excess, U256::ZERO);
        assert!(rewards.iter().all(|r| r.reward_amount == U256::from(500)));
        assert!(mul_div(U256::MAX, U256::MAX, U256::ONE).is_err());
        assert!(mul_div(U256::ONE, U256::ONE, U256::ZERO).is_err());
        let (rewards, excess) =
            calculate_proportional_distribution(U256::ONE, &[(a, U256::ZERO)]).unwrap();
        assert!(rewards.is_empty());
        assert_eq!(excess, U256::ONE);
    }

    #[test]
    fn proportional_distribution_has_no_address_cap() {
        let alice = Address::repeat_byte(1);
        let bob = Address::repeat_byte(2);
        let carol = Address::repeat_byte(3);

        let (rewards, excess) =
            calculate_proportional_distribution(U256::from(100), &[(alice, U256::from(7))])
                .unwrap();
        assert_eq!(excess, U256::ZERO);
        assert_eq!(rewards[0].reward_amount, U256::from(100));

        let (rewards, excess) = calculate_proportional_distribution(
            U256::from(100),
            &[(bob, U256::from(20)), (alice, U256::from(80))],
        )
        .unwrap();
        assert_eq!(excess, U256::ZERO);
        assert_eq!(
            rewards
                .iter()
                .map(|reward| (reward.address, reward.reward_amount))
                .collect::<Vec<_>>(),
            vec![(alice, U256::from(80)), (bob, U256::from(20))]
        );

        let (rewards, excess) = calculate_proportional_distribution(
            U256::from(10),
            &[(carol, U256::ONE), (alice, U256::ONE), (bob, U256::ONE)],
        )
        .unwrap();
        assert_eq!(excess, U256::ONE);
        assert!(rewards
            .iter()
            .all(|reward| reward.reward_amount == U256::from(3)));
        let credited = rewards
            .iter()
            .fold(U256::ZERO, |sum, reward| sum + reward.reward_amount);
        assert_eq!(credited + excess, U256::from(10));

        let (rewards, excess) = calculate_proportional_distribution(U256::from(1000), &[]).unwrap();
        assert!(rewards.is_empty());
        assert_eq!(excess, U256::from(1000));

        let (rewards, excess) = calculate_proportional_distribution(
            U256::from(1000),
            &[(alice, U256::ZERO), (bob, U256::ZERO)],
        )
        .unwrap();
        assert!(rewards.is_empty());
        assert_eq!(excess, U256::from(1000));

        let (rewards, excess) = calculate_proportional_distribution(
            U256::from(100),
            &[
                (carol, U256::ZERO),
                (Address::repeat_byte(4), U256::from(20)),
                (alice, U256::from(80)),
            ],
        )
        .unwrap();
        assert_eq!(
            rewards
                .iter()
                .map(|reward| reward.address)
                .collect::<Vec<_>>(),
            vec![alice, Address::repeat_byte(4)]
        );
        assert_eq!(excess, U256::ZERO);

        assert!(calculate_proportional_distribution(
            U256::ONE,
            &[(alice, U256::MAX), (bob, U256::ONE)]
        )
        .is_err());
    }
}
