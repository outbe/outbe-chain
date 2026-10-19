use crate::errors::AgentRewardError;
use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};
use alloy_primitives::{Address, U256, U512};
use outbe_primitives::error::Result as PrecompileResult;
use outbe_primitives::time::WorldwideDay;

// ------------------------------------------------------------------------
// New daily orchestrator surface
// ------------------------------------------------------------------------

/// One of the three reward pools that AgentReward owns end-to-end. The
/// validator pool is intentionally NOT part of this enum. The EmissionLimit
/// Cycle handler orchestrates validator emission directly against
/// `outbe_rewards::api` for two reasons:
/// * the natural dependency direction is `emissionlimit -> rewards`;
/// * this avoids an `agentreward -> rewards -> emissionlimit -> agentreward`
///   crate cycle.
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
}

/// Daily orchestrator entrypoint called by the EmissionLimit Cycle
/// handler. Dispatches each `(PoolKind, amount)` pair into
/// the correct sub-routine and returns the sum of pool excesses that
/// should be added to the Metadosis terminal credit.
///
/// Excess is rounding residue, or the whole pool when no positive eligible
/// weight exists. Shares are not capped per address.
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
        PoolKind::Waa => (
            RewardPool::Waa,
            contract
                .get_all_waa_counts(prev_day)?
                .into_iter()
                .map(|(address, count)| (address, U256::from(count)))
                .collect(),
        ),
        PoolKind::Sra => (
            RewardPool::Sra,
            contract
                .get_all_sra_counts(prev_day)?
                .into_iter()
                .map(|(address, count)| (address, U256::from(count)))
                .collect(),
        ),
        PoolKind::Cca => (
            RewardPool::Cca,
            outbe_ccaregistry::api::active_reward_weights(&ctx.storage, prev_day.value())?,
        ),
    };
    let (rewards, excess) = calculate_proportional_distribution(amount, &weights)?;
    for reward in rewards {
        if reward.reward_amount.is_zero() {
            continue;
        }
        contract.add_claimable_reward(reward_pool, reward.address, reward.reward_amount)?;
        if reward_pool == RewardPool::Cca {
            contract.emit(IAgentReward::RewardAccrued {
                cca: reward.address,
                utcDay: prev_day.value(),
                amount: reward.reward_amount,
            })?;
        }
    }
    match kind {
        PoolKind::Waa => contract.clear_waa_counts(prev_day)?,
        PoolKind::Sra => contract.clear_sra_counts(prev_day)?,
        PoolKind::Cca => {}
    }
    Ok(excess)
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
