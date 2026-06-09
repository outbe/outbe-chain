use crate::schema::AgentRewardContract;
use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;

/// Maximum share per address (32% cap).
const MAX_ADDRESS_SHARE_PCT: u64 = 32;

/// Maximum redistribution iterations.
const MAX_REDISTRIBUTION_ITERATIONS: usize = 10;

/// Reward allocated to a single address.
pub struct AddressReward {
    pub address: Address,
    pub tribute_count: u64,
    pub reward_amount: U256,
}

/// Distributes a pool with a 32% per-address cap and iterative redistribution.
///
/// The algorithm:
/// 1. Computes proportional shares based on tribute counts.
/// 2. Caps any individual share at 32% of the total pool.
/// 3. Redistributes the excess from capped addresses to uncapped ones,
///    iterating up to `MAX_REDISTRIBUTION_ITERATIONS` times.
///
/// Returns `(rewards, remaining_excess)`. The remaining excess is non-zero
/// only when all addresses are capped and excess cannot be distributed further.
pub fn calculate_distribution_with_cap(
    total_pool: U256,
    counts: &[(Address, u64)],
) -> (Vec<AddressReward>, U256) {
    if counts.is_empty() || total_pool.is_zero() {
        return (vec![], total_pool);
    }

    let total_tributes: u64 = counts.iter().map(|(_, c)| c).sum();
    if total_tributes == 0 {
        return (vec![], total_pool);
    }

    // max_share = total_pool * 32 / 100
    let max_share = total_pool * U256::from(MAX_ADDRESS_SHARE_PCT) / U256::from(100u64);

    let mut sorted_counts = counts.to_vec();
    sorted_counts.sort_by_key(|(addr, _)| *addr);

    // Initial proportional allocation with cap applied immediately.
    // Each entry: (address, tribute_count, current_share, is_capped)
    let mut shares: Vec<(Address, u64, U256, bool)> = sorted_counts
        .iter()
        .map(|(addr, count)| {
            let share = total_pool * U256::from(*count) / U256::from(total_tributes);
            if share > max_share {
                (*addr, *count, max_share, true)
            } else {
                (*addr, *count, share, false)
            }
        })
        .collect();

    // Calculate the initial excess (pool minus what was distributed).
    let total_distributed: U256 = shares
        .iter()
        .map(|(_, _, s, _)| *s)
        .fold(U256::ZERO, |a, b| a + b);
    let mut excess = total_pool.saturating_sub(total_distributed);

    // Iterative redistribution: give excess to uncapped addresses up to their cap.
    for _ in 0..MAX_REDISTRIBUTION_ITERATIONS {
        if excess.is_zero() {
            break;
        }
        let available: U256 = shares
            .iter()
            .filter(|(_, _, share, capped)| !*capped && *share < max_share)
            .map(|(_, _, share, _)| max_share.saturating_sub(*share))
            .fold(U256::ZERO, |acc, v| acc + v);

        if available.is_zero() {
            break;
        }

        for entry in shares.iter_mut() {
            let (_, _, ref mut share, ref mut capped) = entry;
            if *capped || *share >= max_share {
                continue;
            }

            let can_receive = max_share.saturating_sub(*share);
            if can_receive.is_zero() {
                continue;
            }

            let proportional = excess * can_receive / available;
            let to_give = can_receive.min(proportional);
            if to_give.is_zero() {
                continue;
            }
            *share += to_give;
            excess -= to_give;

            if *share >= max_share {
                *capped = true;
            }
            if excess.is_zero() {
                break;
            }
        }

        if excess.is_zero() {
            break;
        }

        let mut dust_distributed = false;
        for entry in shares.iter_mut() {
            let (_, _, ref mut share, ref mut capped) = entry;
            if *capped || *share >= max_share {
                continue;
            }
            let can_receive = max_share.saturating_sub(*share);
            if can_receive.is_zero() {
                continue;
            }
            let to_give = can_receive.min(excess);
            *share += to_give;
            excess -= to_give;
            dust_distributed = true;
            if *share >= max_share {
                *capped = true;
            }
            if excess.is_zero() {
                break;
            }
        }
        if !dust_distributed {
            break;
        }
    }

    let rewards = shares
        .into_iter()
        .map(|(addr, count, share, _)| AddressReward {
            address: addr,
            tribute_count: count,
            reward_amount: share,
        })
        .collect();

    (rewards, excess)
}

// ────────────────────────────────────────────────────────────────────────
// New daily orchestrator surface
// ────────────────────────────────────────────────────────────────────────

/// One of the four reward pools that AgentReward owns end-to-end. The
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
    /// CCA accumulator. The amount is simply added to `CCA_ADDRESS`'s
    /// native balance; there is no distribution logic in v1.
    Cca,
    /// Merchant accumulator. The amount is simply added to
    /// `MERCHANT_ADDRESS`'s native balance; there is no distribution
    /// logic in v1.
    Merchant,
}

/// Daily orchestrator entrypoint called by the EmissionLimit Cycle
/// handler. Dispatches each `(PoolKind, amount)` pair into
/// the correct sub-routine and returns the sum of pool excesses that
/// should be added to the Metadosis terminal credit.
///
/// Excess accounting (per pool kind):
/// * `Waa` / `Sra`: 32 %-cap distribution residue, plus the entire pool
///   when no tributes were recorded for the day (no-tribute case).
/// * `Cca` / `Merchant`: always zero — these pools are pure
///   accumulators on `CCA_ADDRESS` / `MERCHANT_ADDRESS`.
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
    let mut total_excess = U256::ZERO;
    for (kind, amount) in pools {
        let excess = match kind {
            PoolKind::Waa => distribute_capped(ctx, prev_day, PoolKind::Waa, *amount)?,
            PoolKind::Sra => distribute_capped(ctx, prev_day, PoolKind::Sra, *amount)?,
            PoolKind::Cca => {
                accumulate_to_address(ctx, outbe_primitives::addresses::CCA_ADDRESS, *amount)?;
                U256::ZERO
            }
            PoolKind::Merchant => {
                accumulate_to_address(ctx, outbe_primitives::addresses::MERCHANT_ADDRESS, *amount)?;
                U256::ZERO
            }
        };
        total_excess = total_excess.checked_add(excess).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "agentreward distribute_daily overflow".into(),
            )
        })?;
    }
    Ok(total_excess)
}

/// Capped pool flow used for both WAA and SRA. Mints the pool onto
/// `AGENT_REWARD_ADDRESS`, runs the 32 % cap distribution, credits
/// claimable balances, burns the undistributed excess, clears the
/// per-pool day index, and returns the excess.
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
    let mut contract = ctx.contract::<AgentRewardContract>();
    ctx.storage
        .increase_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, amount)?;

    let counts = match kind {
        PoolKind::Waa => contract.get_all_waa_counts(prev_day)?,
        PoolKind::Sra => contract.get_all_sra_counts(prev_day)?,
        _ => unreachable!(),
    };

    if counts.is_empty() {
        // No tributes — burn the pool we just minted so the pre-funded
        // balance does not leak onto AGENT_REWARD_ADDRESS.
        ctx.storage
            .decrease_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, amount)?;
        return Ok(amount);
    }

    let (rewards, excess) = calculate_distribution_with_cap(amount, &counts);
    for r in &rewards {
        if !r.reward_amount.is_zero() {
            contract.add_claimable_reward(r.address, r.reward_amount)?;
        }
    }
    if !excess.is_zero() {
        ctx.storage
            .decrease_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, excess)?;
    }
    match kind {
        PoolKind::Waa => contract.clear_waa_counts(prev_day)?,
        PoolKind::Sra => contract.clear_sra_counts(prev_day)?,
        _ => unreachable!(),
    }
    Ok(excess)
}

/// Mints `amount` onto `target`'s native balance. Used for the CCA and
/// Merchant pools, which are plain accumulators in v1 — no distribution
/// logic, no excess.
fn accumulate_to_address(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    target: alloy_primitives::Address,
    amount: U256,
) -> Result<()> {
    if !amount.is_zero() {
        ctx.storage.increase_balance(target, amount)?;
    }
    Ok(())
}
