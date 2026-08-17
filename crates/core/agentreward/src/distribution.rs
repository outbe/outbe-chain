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
    /// The address's share of the pool before the cap: a tribute count for the
    /// WAA/SRA pools, `units × multiplier` for the CCA pool.
    pub weight: U256,
    pub reward_amount: U256,
}

/// Distributes a pool with a 32% per-address cap and iterative redistribution.
///
/// The algorithm:
/// 1. Computes proportional shares based on each address's weight.
/// 2. Caps any individual share at 32% of the total pool.
/// 3. Redistributes the excess from capped addresses to uncapped ones,
///    iterating up to `MAX_REDISTRIBUTION_ITERATIONS` times.
///
/// Weights are `U256` because the three pools measure participation
/// differently: WAA and SRA count tributes, while the CCA pool weighs
/// 1e18-scaled origination units already multiplied by the agent's performance
/// multiplier. The split itself is weight-agnostic, so all three share it.
///
/// Returns `(rewards, remaining_excess)`. The remaining excess is non-zero
/// only when all addresses are capped and excess cannot be distributed further.
pub fn calculate_distribution_with_cap(
    total_pool: U256,
    weights: &[(Address, U256)],
) -> (Vec<AddressReward>, U256) {
    if weights.is_empty() || total_pool.is_zero() {
        return (vec![], total_pool);
    }

    let total_weight = weights
        .iter()
        .fold(U256::ZERO, |acc, (_, w)| acc.saturating_add(*w));
    if total_weight.is_zero() {
        return (vec![], total_pool);
    }

    // max_share = total_pool * 32 / 100
    let max_share = total_pool * U256::from(MAX_ADDRESS_SHARE_PCT) / U256::from(100u64);

    let mut sorted_weights = weights.to_vec();
    sorted_weights.sort_by_key(|(addr, _)| *addr);

    // Initial proportional allocation with cap applied immediately.
    // Each entry: (address, weight, current_share, is_capped)
    let mut shares: Vec<(Address, U256, U256, bool)> = sorted_weights
        .iter()
        .map(|(addr, weight)| {
            // ponytail: saturating rather than a 512-bit intermediate. A daily
            // pool is ~1e24 wei and the largest weight ~1e20 (a hundred
            // 1e18-scaled units), so the product sits ~1e44 against a U256
            // ceiling of ~1e77. Widen this if either ever grows by 15 orders of
            // magnitude; the previous plain `*` would have panicked instead.
            let share = total_pool.saturating_mul(*weight) / total_weight;
            if share > max_share {
                (*addr, *weight, max_share, true)
            } else {
                (*addr, *weight, share, false)
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
        .map(|(addr, weight, share, _)| AddressReward {
            address: addr,
            weight,
            reward_amount: share,
        })
        .collect();

    (rewards, excess)
}

/// Tribute counts as distribution weights. WAA and SRA weigh participation by
/// tribute count; the split arithmetic itself does not care which unit it is.
fn counts_as_weights(counts: Vec<(Address, u64)>) -> Vec<(Address, U256)> {
    counts
        .into_iter()
        .map(|(addr, count)| (addr, U256::from(count)))
        .collect()
}

// ────────────────────────────────────────────────────────────────────────
// New daily orchestrator surface
// ────────────────────────────────────────────────────────────────────────

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
    /// CCA (Credis Card Agent) capped distribution pool. Weighs each agent by
    /// the origination units it earned that day times its performance
    /// multiplier, both read from the CCA registry.
    Cca,
}

/// Daily orchestrator entrypoint called by the EmissionLimit Cycle
/// handler. Dispatches each `(PoolKind, amount)` pair into
/// the correct sub-routine and returns the sum of pool excesses that
/// should be added to the Metadosis terminal credit.
///
/// Excess accounting is the same for all three pools: the 32 %-cap
/// distribution residue, plus the entire pool when nobody participated
/// that day. The `Cca` pool additionally carries the one-time
/// pre-activation sweep the first time it runs.
///
/// Mint/burn parity is enforced inside [`distribute_capped`]: each pool
/// is minted onto `AGENT_REWARD_ADDRESS` before distribution, and the
/// undistributed excess is burned back, so
/// `balance(AGENT_REWARD_ADDRESS)` after the call equals the total
/// claimable credited for that day.
pub fn distribute_daily(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
    prev_day: WorldwideDay,
    pools: &[(PoolKind, U256)],
) -> Result<U256> {
    let mut total_excess = U256::ZERO;
    for (kind, amount) in pools {
        let mut excess = distribute_capped(ctx, prev_day, *kind, *amount)?;
        if matches!(kind, PoolKind::Cca) {
            excess = add(excess, sweep_pre_activation_cca_sink(ctx)?)?;
        }
        total_excess = add(total_excess, excess)?;
    }
    Ok(total_excess)
}

fn add(left: U256, right: U256) -> Result<U256> {
    left.checked_add(right).ok_or_else(|| {
        outbe_primitives::error::PrecompileError::Revert(
            "agentreward distribute_daily overflow".into(),
        )
    })
}

/// Moves whatever the CCA sink accumulated before this program existed to the
/// Metadosis sink, once, and returns the amount moved.
///
/// §8.3: those balances are not distributed retroactively. `PoolKind::Cca` used
/// to mint straight onto `CCA_ADDRESS` with nobody able to spend it, so the
/// balance standing there on the first distributing day is exactly that
/// pre-activation residue. It is burned here and returned as excess, taking the
/// same route to Metadosis as any undistributed pool.
///
/// The one-shot flag is what makes it a sweep rather than a recurring drain: on
/// every later day the sink is no longer credited, so this must not run again.
/// The product paper also names a Merchant sink; no such address exists in this
/// repository, so there is nothing to sweep for it.
fn sweep_pre_activation_cca_sink(
    ctx: &outbe_primitives::block::BlockRuntimeContext,
) -> Result<U256> {
    let contract = ctx.contract::<AgentRewardContract>();
    if contract.cca_sink_swept.read()? {
        return Ok(U256::ZERO);
    }
    contract.cca_sink_swept.write(true)?;

    let sink = outbe_primitives::addresses::CCA_ADDRESS;
    let accumulated = ctx.storage.balance(sink)?;
    if accumulated.is_zero() {
        return Ok(U256::ZERO);
    }
    ctx.storage.decrease_balance(sink, accumulated)?;
    tracing::info!(
        target: "outbe::agentreward",
        amount = %accumulated,
        "swept the pre-activation CCA sink balance to the Metadosis terminal"
    );
    Ok(accumulated)
}

/// Capped pool flow shared by all three pools. Mints the pool onto
/// `AGENT_REWARD_ADDRESS`, runs the 32 % cap distribution over that pool's
/// weights, credits claimable balances, burns the undistributed excess, clears
/// the day's participation record, and returns the excess.
///
/// Only the weight source and the clear differ between pools; the money
/// movement, the cap and the parity burn are identical, so they live here once.
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
    ctx.storage
        .increase_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, amount)?;

    let weights = match kind {
        PoolKind::Waa => counts_as_weights(contract.get_all_waa_counts(prev_day)?),
        PoolKind::Sra => counts_as_weights(contract.get_all_sra_counts(prev_day)?),
        PoolKind::Cca => outbe_cca::api::day_reward_weights(ctx.storage.clone(), prev_day)?,
    };

    if weights.is_empty() {
        // Nobody participated — burn the pool we just minted so the pre-funded
        // balance does not leak onto AGENT_REWARD_ADDRESS.
        ctx.storage
            .decrease_balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS, amount)?;
        return Ok(amount);
    }

    let (rewards, excess) = calculate_distribution_with_cap(amount, &weights);
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
        PoolKind::Cca => outbe_cca::api::settle_day(ctx.storage.clone(), prev_day)?,
    }
    Ok(excess)
}
