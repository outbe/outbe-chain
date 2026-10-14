use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};
use alloy_primitives::{Address, U256, U512};
use outbe_primitives::error::{PrecompileError, Result as PrecompileResult};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::units::checked_protocol_to_native;

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
    /// WAA (wallet) capped distribution pool. Uses tribute counts kept
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
/// Excess accounting (per pool kind):
/// * `Waa`: 32 %-cap distribution residue, or the whole pool with no tributes.
/// * `Sra` / `Cca`: rounding residue only, or the whole pool when no positive
///   eligible weight exists. Shares are not capped per address.
///
/// The function mints backing at AGENT_REWARD_ADDRESS only for credited rewards.
/// Claimable balances use native units; returned excess uses protocol units.
pub fn distribute_daily(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> PrecompileResult<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut total_excess = U256::ZERO;
        for (kind, amount) in pools {
            let excess = distribute_pool(ctx, prev_day, *kind, *amount)?;
            total_excess = total_excess.checked_add(excess).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Revert(
                    "agentreward distribute_daily overflow".into(),
                )
            })?;
        }
        Ok(total_excess)
    })
}

/// Allocates one agent pool. WAA uses the per-address cap. SRA and CCA are
/// proportional. One conversion to native COEN backs each credited share.
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
    let (rewards, excess) = match kind {
        PoolKind::Waa => calculate_distribution_with_cap(amount, &weights),
        PoolKind::Sra | PoolKind::Cca => calculate_proportional_distribution(amount, &weights),
    }
    .map_err(|error| PrecompileError::Revert(error.to_string()))?;
    for reward in rewards {
        if reward.reward_amount.is_zero() {
            continue;
        }
        let native = checked_protocol_to_native(reward.reward_amount)
            .ok_or_else(|| PrecompileError::Revert("native AgentReward share overflow".into()))?;
        contract.add_claimable_reward(reward_pool, reward.address, native)?;
        ctx.storage
            .increase_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, native)?;
        if reward_pool == RewardPool::Cca {
            contract.emit(IAgentReward::RewardAccrued {
                cca: reward.address,
                utcDay: prev_day.value(),
                amount: native,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("reward distribution arithmetic overflow")]
pub struct DistributionError;

fn mul_div(a: U256, b: U256, denominator: U256) -> Result<U256, DistributionError> {
    // A product of two U256 values fits U512 without truncation.
    let value = (U512::from(a) * U512::from(b))
        .checked_div(U512::from(denominator))
        .ok_or(DistributionError)?;
    if value > U512::from(U256::MAX) {
        return Err(DistributionError);
    }
    // The range check above proves the narrowing conversion is lossless.
    Ok(value.wrapping_to::<U256>())
}

/// Maximum share per address (32% cap).
const MAX_ADDRESS_SHARE_PCT: u64 = 32;

/// Maximum redistribution iterations.
const MAX_REDISTRIBUTION_ITERATIONS: usize = 10;

/// Reward allocated to a single address.
pub struct AddressReward {
    pub address: Address,
    pub weight: U256,
    pub reward_amount: U256,
}

/// Distributes a pool with a 32% per-address cap and iterative redistribution.
///
/// The algorithm:
/// 1. Computes proportional shares based on weights.
/// 2. Caps any individual share at 32% of the total pool.
/// 3. Redistributes the excess from capped addresses to uncapped ones,
///    iterating up to `MAX_REDISTRIBUTION_ITERATIONS` times.
///
/// Returns `(rewards, remaining_excess)`, including cap residue and any amount
/// left when there are no positive weights or the iteration bound is reached.
/// Callers supply one weight per distinct address.
pub fn calculate_distribution_with_cap(
    total_pool: U256,
    counts: &[(Address, U256)],
) -> Result<(Vec<AddressReward>, U256), DistributionError> {
    if counts.is_empty() || total_pool.is_zero() {
        return Ok((vec![], total_pool));
    }

    let total_tributes = counts.iter().try_fold(U256::ZERO, |sum, (_, weight)| {
        sum.checked_add(*weight).ok_or(DistributionError)
    })?;
    if total_tributes.is_zero() {
        return Ok((vec![], total_pool));
    }

    // max_share = total_pool * 32 / 100
    let max_share = mul_div(
        total_pool,
        U256::from(MAX_ADDRESS_SHARE_PCT),
        U256::from(100u64),
    )?;

    let mut shares = initial_capped_shares(total_pool, counts, total_tributes, max_share)?;
    let total_distributed = shares.iter().try_fold(U256::ZERO, |sum, entry| {
        sum.checked_add(entry.share).ok_or(DistributionError)
    })?;
    let mut excess = total_pool
        .checked_sub(total_distributed)
        .ok_or(DistributionError)?;

    // Iterative redistribution: give excess to uncapped addresses up to their cap.
    for _ in 0..MAX_REDISTRIBUTION_ITERATIONS {
        if excess.is_zero() {
            break;
        }
        let available = shares.iter().try_fold(U256::ZERO, |sum, entry| {
            let room = entry.room(max_share).unwrap_or_default();
            sum.checked_add(room).ok_or(DistributionError)
        })?;
        if available.is_zero() {
            break;
        }
        excess = give_proportionally(&mut shares, excess, available, max_share)?;
        if excess.is_zero() {
            break;
        }
        let dust_distributed;
        (excess, dust_distributed) = give_dust(&mut shares, excess, max_share)?;
        if !dust_distributed {
            break;
        }
    }

    let rewards = shares
        .into_iter()
        .map(|entry| AddressReward {
            address: entry.address,
            weight: entry.weight,
            reward_amount: entry.share,
        })
        .collect();

    Ok((rewards, excess))
}

struct CappedShare {
    address: Address,
    weight: U256,
    share: U256,
    capped: bool,
}

impl CappedShare {
    fn room(&self, max_share: U256) -> Option<U256> {
        if self.capped || self.share >= max_share {
            return None;
        }
        Some(max_share.saturating_sub(self.share))
    }

    fn credit(
        &mut self,
        amount: U256,
        excess: U256,
        max_share: U256,
    ) -> Result<U256, DistributionError> {
        self.share = self.share.checked_add(amount).ok_or(DistributionError)?;
        let excess = excess.checked_sub(amount).ok_or(DistributionError)?;
        if self.share >= max_share {
            self.capped = true;
        }
        Ok(excess)
    }
}

fn initial_capped_shares(
    total_pool: U256,
    counts: &[(Address, U256)],
    total_tributes: U256,
    max_share: U256,
) -> Result<Vec<CappedShare>, DistributionError> {
    let mut positive: Vec<_> = counts
        .iter()
        .copied()
        .filter(|(_, weight)| !weight.is_zero())
        .collect();
    positive.sort_by_key(|(addr, _)| *addr);
    positive
        .into_iter()
        .map(|(address, weight)| {
            let share = mul_div(total_pool, weight, total_tributes)?;
            Ok(CappedShare {
                address,
                weight,
                share: share.min(max_share),
                capped: share > max_share,
            })
        })
        .collect()
}

/// Each address's part is taken from the excess left after the addresses before it.
fn give_proportionally(
    shares: &mut [CappedShare],
    mut excess: U256,
    available: U256,
    max_share: U256,
) -> Result<U256, DistributionError> {
    for entry in shares.iter_mut() {
        let Some(room) = entry.room(max_share) else {
            continue;
        };
        let to_give = room.min(mul_div(excess, room, available)?);
        if to_give.is_zero() {
            continue;
        }
        excess = entry.credit(to_give, excess, max_share)?;
        if excess.is_zero() {
            break;
        }
    }
    Ok(excess)
}

fn give_dust(
    shares: &mut [CappedShare],
    mut excess: U256,
    max_share: U256,
) -> Result<(U256, bool), DistributionError> {
    let mut distributed = false;
    for entry in shares.iter_mut() {
        let Some(room) = entry.room(max_share) else {
            continue;
        };
        excess = entry.credit(room.min(excess), excess, max_share)?;
        distributed = true;
        if excess.is_zero() {
            break;
        }
    }
    Ok((excess, distributed))
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
) -> Result<(Vec<AddressReward>, U256), DistributionError> {
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
        sum.checked_add(*weight).ok_or(DistributionError)
    })?;

    let mut rewards = Vec::with_capacity(positive.len());
    let mut distributed = U256::ZERO;
    for (addr, weight) in positive {
        let share = mul_div(total_pool, weight, total_weight)?;
        distributed = distributed.checked_add(share).ok_or(DistributionError)?;
        rewards.push(AddressReward {
            address: addr,
            weight,
            reward_amount: share,
        });
    }
    let excess = total_pool
        .checked_sub(distributed)
        .ok_or(DistributionError)?;
    Ok((rewards, excess))
}

#[cfg(test)]
mod tests;
