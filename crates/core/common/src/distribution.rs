//! Deterministic capped reward allocation in protocol units.
use alloy_primitives::{Address, U256, U512};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("capped reward distribution arithmetic overflow")]
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

    let mut sorted_counts: Vec<_> = counts
        .iter()
        .copied()
        .filter(|(_, weight)| !weight.is_zero())
        .collect();
    sorted_counts.sort_by_key(|(addr, _)| *addr);

    // Initial proportional allocation with cap applied immediately.
    // Each entry: (address, weight, current_share, is_capped)
    let mut shares: Vec<(Address, U256, U256, bool)> = sorted_counts
        .iter()
        .map(|(addr, count)| {
            let share = mul_div(total_pool, *count, total_tributes)?;
            if share > max_share {
                Ok((*addr, *count, max_share, true))
            } else {
                Ok((*addr, *count, share, false))
            }
        })
        .collect::<Result<_, DistributionError>>()?;

    // Calculate the initial excess (pool minus what was distributed).
    let total_distributed: U256 = shares
        .iter()
        .map(|(_, _, s, _)| *s)
        .try_fold(U256::ZERO, |a, b| a.checked_add(b).ok_or(DistributionError))?;
    let mut excess = total_pool
        .checked_sub(total_distributed)
        .ok_or(DistributionError)?;

    // Iterative redistribution: give excess to uncapped addresses up to their cap.
    for _ in 0..MAX_REDISTRIBUTION_ITERATIONS {
        if excess.is_zero() {
            break;
        }
        let available: U256 = shares
            .iter()
            .filter(|(_, _, share, capped)| !*capped && *share < max_share)
            .map(|(_, _, share, _)| max_share.saturating_sub(*share))
            .try_fold(U256::ZERO, |acc, v| {
                acc.checked_add(v).ok_or(DistributionError)
            })?;

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

            let proportional = mul_div(excess, can_receive, available)?;
            let to_give = can_receive.min(proportional);
            if to_give.is_zero() {
                continue;
            }
            *share = share.checked_add(to_give).ok_or(DistributionError)?;
            excess = excess.checked_sub(to_give).ok_or(DistributionError)?;

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
            *share = share.checked_add(to_give).ok_or(DistributionError)?;
            excess = excess.checked_sub(to_give).ok_or(DistributionError)?;
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
            weight: count,
            reward_amount: share,
        })
        .collect();

    Ok((rewards, excess))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    // ---------------------------------------------------------------------------
    // calculate_distribution_with_cap unit tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_distribution_single_address() {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let pool = U256::from(1000u64);
        let counts = vec![(alice, U256::from(10))];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        // Single address with 100% of tributes: capped at 32%.
        assert_eq!(rewards.len(), 1);
        assert_eq!(rewards[0].address, alice);
        // Cap: 1000 * 32 / 100 = 320
        let expected = U256::from(320u64);
        assert_eq!(rewards[0].reward_amount, expected);
        // Excess = 1000 - 320 = 680
        assert_eq!(excess, U256::from(680u64));
    }

    #[test]
    fn test_distribution_equal_shares() {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");
        let carol = address!("0x3333333333333333333333333333333333333333");
        let dave = address!("0x4444444444444444444444444444444444444444");

        // 4 addresses with equal tributes, each gets 25%.
        // 25% < 32% cap so no capping; all pool is distributed.
        let pool = U256::from(1000u64);
        let counts = vec![
            (alice, U256::ONE),
            (bob, U256::ONE),
            (carol, U256::ONE),
            (dave, U256::ONE),
        ];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert_eq!(rewards.len(), 4);
        // Each gets 1000 * 1 / 4 = 250
        for r in &rewards {
            assert_eq!(r.reward_amount, U256::from(250u64));
        }
        // Integer division: 4 * 250 = 1000, no rounding loss here.
        assert_eq!(excess, U256::ZERO);
    }

    #[test]
    fn test_distribution_with_cap() {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        // Alice has 9 tributes, Bob has 1 - Alice would get 90% but is capped at 32%.
        // Excess (58%) is redistributed to Bob who is uncapped; Bob ends up at 32%
        // as well because 68% > 32%. Final excess = 100% - 32% - 32% = 36%.
        let pool = U256::from(1000u64);
        let counts = vec![(alice, U256::from(9)), (bob, U256::ONE)];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert_eq!(rewards.len(), 2);

        let alice_reward = rewards.iter().find(|r| r.address == alice).unwrap();
        let bob_reward = rewards.iter().find(|r| r.address == bob).unwrap();

        // Both capped at 320 (32% of 1000).
        assert_eq!(alice_reward.reward_amount, U256::from(320u64));
        assert_eq!(bob_reward.reward_amount, U256::from(320u64));
        assert_eq!(excess, U256::from(360u64));
    }

    #[test]
    fn test_distribution_all_capped() {
        // 4 addresses, each with equal tributes.
        // Total pool = 1000, each would proportionally get 250 (25%), under the 32% cap.
        // No capping occurs, all pool is distributed.
        let a = address!("0x1111111111111111111111111111111111111111");
        let b = address!("0x2222222222222222222222222222222222222222");
        let c = address!("0x3333333333333333333333333333333333333333");
        let d = address!("0x4444444444444444444444444444444444444444");

        let pool = U256::from(1000u64);
        let counts = vec![
            (a, U256::from(25)),
            (b, U256::from(25)),
            (c, U256::from(25)),
            (d, U256::from(25)),
        ];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert_eq!(rewards.len(), 4);
        for r in &rewards {
            assert_eq!(r.reward_amount, U256::from(250u64));
        }
        assert_eq!(excess, U256::ZERO);
    }

    #[test]
    fn test_distribution_all_capped_with_excess() {
        // 3 addresses with exactly equal shares - 33.3% each, all exceed 32% cap.
        // After capping: each gets 32%, total = 96%, excess = 4%.
        // Redistribution cannot help (all capped), so excess stays.
        let a = address!("0x1111111111111111111111111111111111111111");
        let b = address!("0x2222222222222222222222222222222222222222");
        let c = address!("0x3333333333333333333333333333333333333333");

        let pool = U256::from(300u64);
        let counts = vec![(a, U256::ONE), (b, U256::ONE), (c, U256::ONE)];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert_eq!(rewards.len(), 3);
        // max_share = 300 * 32 / 100 = 96
        // proportional share = 300 * 1 / 3 = 100 > 96, so capped.
        for r in &rewards {
            assert_eq!(r.reward_amount, U256::from(96u64));
        }
        // excess = 300 - 3*96 = 300 - 288 = 12
        assert_eq!(excess, U256::from(12u64));
    }

    #[test]
    fn test_distribution_empty_counts() {
        let pool = U256::from(1000u64);
        let counts: Vec<(alloy_primitives::Address, U256)> = vec![];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert!(rewards.is_empty());
        // Full pool returned as excess.
        assert_eq!(excess, pool);
    }

    #[test]
    fn test_distribution_zero_pool() {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let pool = U256::ZERO;
        let counts = vec![(alice, U256::from(5))];

        let (rewards, excess) = calculate_distribution_with_cap(pool, &counts).unwrap();

        assert!(rewards.is_empty());
        assert_eq!(excess, U256::ZERO);
    }

    #[test]
    fn redistribution_preserves_address_order_and_dust() {
        let weights: Vec<_> = [(4, 2), (2, 5), (1, 90), (3, 3), (5, 0)]
            .into_iter()
            .map(|(address, weight)| (Address::repeat_byte(address), U256::from(weight)))
            .collect();
        let (rewards, excess) =
            calculate_distribution_with_cap(U256::from(1000), &weights).unwrap();
        assert_eq!(excess, U256::ZERO);
        assert_eq!(
            rewards.iter().map(|r| r.reward_amount).collect::<Vec<_>>(),
            [320, 320, 248, 112].map(U256::from)
        );
        assert_eq!(
            rewards.iter().map(|r| r.address).collect::<Vec<_>>(),
            [1, 2, 3, 4].map(Address::repeat_byte)
        );
        let weights: Vec<_> = (1..=4)
            .map(|i| (Address::repeat_byte(i), U256::ONE))
            .collect();
        let (rewards, excess) = calculate_distribution_with_cap(U256::from(11), &weights).unwrap();
        assert_eq!(excess, U256::ZERO);
        assert_eq!(
            rewards.iter().map(|r| r.reward_amount).collect::<Vec<_>>(),
            [3, 3, 3, 2].map(U256::from)
        );
        let (rewards, excess) = calculate_distribution_with_cap(U256::ONE, &weights).unwrap();
        assert_eq!(excess, U256::ONE);
        assert!(rewards.iter().all(|r| r.reward_amount.is_zero()));
    }

    #[test]
    fn wide_products_and_checked_overflow() {
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let (rewards, excess) =
            calculate_distribution_with_cap(U256::MAX, &[(a, U256::MAX)]).unwrap();
        let cap = (U512::from(U256::MAX) * U512::from(32) / U512::from(100)).wrapping_to::<U256>();
        assert_eq!(rewards[0].reward_amount, cap);
        assert_eq!(rewards[0].weight, U256::MAX);
        assert_eq!(excess, U256::MAX - cap);
        assert!(
            calculate_distribution_with_cap(U256::ONE, &[(a, U256::MAX), (b, U256::ONE)]).is_err()
        );
        let (rewards, excess) = calculate_distribution_with_cap(
            U256::from(1000),
            &[(a, U256::from(u64::MAX)), (b, U256::from(u64::MAX))],
        )
        .unwrap();
        assert_eq!(excess, U256::from(360));
        assert!(rewards.iter().all(|r| r.reward_amount == U256::from(320)));
        assert!(mul_div(U256::MAX, U256::MAX, U256::ONE).is_err());
        assert!(mul_div(U256::ONE, U256::ONE, U256::ZERO).is_err());
        let (rewards, excess) =
            calculate_distribution_with_cap(U256::ONE, &[(a, U256::ZERO)]).unwrap();
        assert!(rewards.is_empty());
        assert_eq!(excess, U256::ONE);
    }
}
