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
    // 25% < 32% cap, so no capping occurs. The function distributes all of the pool.
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
    // The function redistributes the excess (58%) to Bob, who is uncapped. Bob is also
    // capped at 32% because 68% > 32%. Final excess = 100% - 32% - 32% = 36%.
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
    // No capping occurs. The function distributes all of the pool.
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
    // 3 addresses with exactly equal shares. Each has 33.3%, and all exceed the 32% cap.
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
    // The function returns the full pool as excess.
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
    let (rewards, excess) = calculate_distribution_with_cap(U256::from(1000), &weights).unwrap();
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
    let (rewards, excess) = calculate_distribution_with_cap(U256::MAX, &[(a, U256::MAX)]).unwrap();
    let cap = (U512::from(U256::MAX) * U512::from(32) / U512::from(100)).wrapping_to::<U256>();
    assert_eq!(rewards[0].reward_amount, cap);
    assert_eq!(rewards[0].weight, U256::MAX);
    assert_eq!(excess, U256::MAX - cap);
    assert!(calculate_distribution_with_cap(U256::ONE, &[(a, U256::MAX), (b, U256::ONE)]).is_err());
    let (rewards, excess) = calculate_distribution_with_cap(
        U256::from(1000),
        &[(a, U256::from(u64::MAX)), (b, U256::from(u64::MAX))],
    )
    .unwrap();
    assert_eq!(excess, U256::from(360));
    assert!(rewards.iter().all(|r| r.reward_amount == U256::from(320)));
    assert!(mul_div(U256::MAX, U256::MAX, U256::ONE).is_err());
    assert!(mul_div(U256::ONE, U256::ONE, U256::ZERO).is_err());
    let (rewards, excess) = calculate_distribution_with_cap(U256::ONE, &[(a, U256::ZERO)]).unwrap();
    assert!(rewards.is_empty());
    assert_eq!(excess, U256::ONE);
}

#[test]
fn proportional_distribution_has_no_address_cap() {
    let alice = Address::repeat_byte(1);
    let bob = Address::repeat_byte(2);
    let carol = Address::repeat_byte(3);

    let (rewards, excess) =
        calculate_proportional_distribution(U256::from(100), &[(alice, U256::from(7))]).unwrap();
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
