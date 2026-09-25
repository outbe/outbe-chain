use crate::precompile::IAgentReward;
use crate::schema::{AgentRewardContract, RewardPool};
use alloy_primitives::U256;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::units::checked_protocol_to_native;

use outbe_common::distribution::calculate_distribution_with_cap;

// ------------------------------------------------------------------------
// New daily orchestrator surface
// ------------------------------------------------------------------------

/// One of the three reward pools that AgentReward owns end-to-end. The
/// validator pool is intentionally NOT part of this enum: validator
/// emission is orchestrated by the EmissionLimit Cycle handler
/// directly against `outbe_rewards::api`, both because the
/// natural dependency direction is `emissionlimit -> rewards` and to
/// avoid an `agentreward -> rewards -> emissionlimit -> agentreward`
/// crate cycle.
///
/// The split between pool kinds happens in the EmissionLimit daily
/// handler; this enum is the protocol contract between that
/// orchestrator and AgentReward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolKind {
    /// WAA (wallet) capped distribution pool. Uses tribute counts kept
    /// in `waa_*` storage fields.
    Waa,
    /// SRA (signer-of-record-attestation) capped distribution pool. Uses
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
/// * `Waa` / `Sra`: 32 %-cap distribution residue, plus the entire pool
///   when no tributes were recorded for the day (no-tribute case).
/// * `Cca`: cap and rounding residue, or the whole pool when no active weight exists.
///
/// Backing is minted at AGENT_REWARD_ADDRESS only for credited rewards.
/// Claimable balances use native units; returned excess uses protocol units.
pub fn distribute_daily(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> Result<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut total_excess = U256::ZERO;
        for (kind, amount) in pools {
            let excess = distribute_capped(ctx, prev_day, *kind, *amount)?;
            total_excess = total_excess.checked_add(excess).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Revert(
                    "agentreward distribute_daily overflow".into(),
                )
            })?;
        }
        Ok(total_excess)
    })
}

/// Shared capped allocation for all agent pools. One conversion to native
/// COEN backs each credited share; undistributed protocol units are returned.
fn distribute_capped(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    kind: PoolKind,
    amount: U256,
) -> Result<U256> {
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
    let (rewards, excess) = calculate_distribution_with_cap(amount, &weights)
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
