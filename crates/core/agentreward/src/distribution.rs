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
    /// CCA weighted origination rewards, owned by the CCA registry.
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
/// Mint/burn parity is enforced inside the WAA/SRA helpers: each pool
/// is minted onto `AGENT_REWARD_ADDRESS` before distribution, and the
/// undistributed excess (cap or no-tribute) is burned back, so
/// `balance(AGENT_REWARD_ADDRESS)` after the call equals the total
/// claimable credited for that day.
pub fn distribute_daily(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> Result<U256> {
    ctx.storage.with_checkpoint(|| {
        let mut total_excess = U256::ZERO;
        for (kind, amount) in pools {
            let excess = match kind {
                PoolKind::Waa => distribute_capped(ctx, prev_day, PoolKind::Waa, *amount)?,
                PoolKind::Sra => distribute_capped(ctx, prev_day, PoolKind::Sra, *amount)?,
                PoolKind::Cca => outbe_ccaregistry::emission_sink::distribute_daily(
                    ctx,
                    prev_day.value(),
                    *amount,
                )?,
            };
            total_excess = total_excess.checked_add(excess).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Revert(
                    "agentreward distribute_daily overflow".into(),
                )
            })?;
        }
        Ok(total_excess)
    })
}

/// Capped pool flow used for both WAA and SRA. Distribution math and the
/// returned excess stay in six-decimal emission units. Native backing and
/// claimable balances cross the boundary once into 18-decimal COEN.
fn distribute_capped(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    kind: PoolKind,
    amount: U256,
) -> Result<U256> {
    debug_assert!(matches!(kind, PoolKind::Waa | PoolKind::Sra));
    if amount.is_zero() {
        return Ok(U256::ZERO);
    }
    let native_amount = checked_protocol_to_native(amount)
        .ok_or_else(|| PrecompileError::Revert("native AgentReward pool overflow".into()))?;
    let mut contract = ctx.contract::<AgentRewardContract>();
    ctx.storage.increase_balance(
        outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
        native_amount,
    )?;

    let counts = match kind {
        PoolKind::Waa => contract.get_all_waa_counts(prev_day)?,
        PoolKind::Sra => contract.get_all_sra_counts(prev_day)?,
        _ => unreachable!(),
    };

    if counts.is_empty() {
        // No tributes - burn the pool we just minted so the pre-funded
        // balance does not leak onto AGENT_REWARD_ADDRESS.
        ctx.storage.decrease_balance(
            outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
            native_amount,
        )?;
        return Ok(amount);
    }

    let reward_pool = match kind {
        PoolKind::Waa => RewardPool::Waa,
        PoolKind::Sra => RewardPool::Sra,
        _ => unreachable!(),
    };
    let weights: Vec<_> = counts
        .into_iter()
        .map(|(address, count)| (address, U256::from(count)))
        .collect();
    let (rewards, excess) = calculate_distribution_with_cap(amount, &weights)
        .map_err(|error| PrecompileError::Revert(error.to_string()))?;
    for r in &rewards {
        if !r.reward_amount.is_zero() {
            let native_reward = checked_protocol_to_native(r.reward_amount).ok_or_else(|| {
                PrecompileError::Revert("native AgentReward share overflow".into())
            })?;
            contract.add_claimable_reward(reward_pool, r.address, native_reward)?;
        }
    }
    if !excess.is_zero() {
        let native_excess = checked_protocol_to_native(excess)
            .ok_or_else(|| PrecompileError::Revert("native AgentReward excess overflow".into()))?;
        ctx.storage.decrease_balance(
            outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
            native_excess,
        )?;
    }
    match kind {
        PoolKind::Waa => contract.clear_waa_counts(prev_day)?,
        PoolKind::Sra => contract.clear_sra_counts(prev_day)?,
        _ => unreachable!(),
    }
    Ok(excess)
}
