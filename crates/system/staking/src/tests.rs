use alloy_primitives::{address, Address, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::logic::status;

use crate::contract::Staking;
use crate::hooks;

const CHAIN_ID: u64 = 1;
const MIN_STAKE: u64 = 1_000;

/// Default large balance seeded to callers so transfer_balance succeeds.
const DEFAULT_BALANCE: u64 = 1_000_000;

fn with_staking<R>(f: impl FnOnce(StorageHandle, &mut Staking) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        // Set a default min stake for tests
        s.config_min_stake
            .write(U256::from(MIN_STAKE))
            .expect("write min_stake");
        s.config_unbonding_period
            .write(3600)
            .expect("write unbonding_period");
        f(storage, &mut s)
    })
}

fn with_staking_timed<R>(timestamp: u64, f: impl FnOnce(StorageHandle, &mut Staking) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(timestamp));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake
            .write(U256::from(MIN_STAKE))
            .expect("write min_stake");
        s.config_unbonding_period
            .write(3600)
            .expect("write unbonding_period");
        f(storage, &mut s)
    })
}

/// Seed a caller's native balance so transfer_balance in stake() succeeds.
fn seed_balance(storage: StorageHandle, addr: Address, amount: u64) {
    let ctx = storage.clone();
    ctx.set_balance(addr, U256::from(amount)).unwrap();
}

/// Registers a validator in ValidatorSet so cross-calls work correctly.
/// Uses owner registration path to bypass A-45 BLS proof-of-key requirement.
fn register_validator(storage: StorageHandle, validator: Address) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    let mut val_set = ValidatorSet::new(storage.clone());
    val_set.config_owner.write(owner).expect("write owner");
    val_set.config_max_validators.write(100).expect("write max");
    val_set
        .register_validator(owner, validator, &[0u8; 48])
        .expect("register_validator");
}

/// Seeds STAKING_ADDRESS with balance (simulating EVM-level msg.value transfer).
/// A-01: stake() no longer transfers; in production EVM does it.
fn seed_staking_balance(storage: StorageHandle, amount: u64) {
    let ctx = storage.clone();
    let current = ctx.balance(STAKING_ADDRESS).unwrap();
    ctx.set_balance(STAKING_ADDRESS, current + U256::from(amount))
        .unwrap();
}

// ---------------------------------------------------------------------------
// test_stake
// ---------------------------------------------------------------------------

#[test]
fn test_stake() {
    with_staking(|storage, s| {
        // A-43: Self-stake only (caller == validator)
        let validator = address!("0x1111111111111111111111111111111111111111");
        let amount = U256::from(500u64);

        // A-01: stake() doesn't transfer funds; in production EVM does it.
        // Seed STAKING_ADDRESS to simulate EVM msg.value transfer.
        seed_staking_balance(storage.clone(), 500);
        s.stake(validator, validator, amount).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), amount);
        assert_eq!(s.get_total_staked().unwrap(), amount);
    });
}

#[test]
fn test_stake_third_party_rejected() {
    with_staking(|_storage, s| {
        // A-43: Third-party staking is no longer supported
        let validator = address!("0x1111111111111111111111111111111111111111");
        let caller = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(s.stake(caller, validator, U256::from(500u64)).is_err());
    });
}

#[test]
fn test_stake_accumulates() {
    with_staking(|storage, s| {
        let validator = address!("0x1111111111111111111111111111111111111111");

        seed_staking_balance(storage.clone(), 1_000);
        s.stake(validator, validator, U256::from(300u64)).unwrap();
        s.stake(validator, validator, U256::from(700u64)).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), U256::from(1_000u64));
        assert_eq!(s.get_total_staked().unwrap(), U256::from(1_000u64));
    });
}

#[test]
fn test_stake_activates_registered_validator() {
    with_staking(|storage, s| {
        let validator = address!("0x2222222222222222222222222222222222222222");
        register_validator(storage.clone(), validator);

        // Check initial status is REGISTERED
        let val_set = ValidatorSet::new(storage.clone());
        let pre_status = val_set.val_status.read(&validator).unwrap();
        assert_eq!(pre_status, status::REGISTERED);

        // Stake enough to meet min_stake
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        s.stake(validator, validator, U256::from(MIN_STAKE))
            .unwrap();

        // Validator should now be ACTIVE
        let val_set = ValidatorSet::new(storage.clone());
        let post_status = val_set.val_status.read(&validator).unwrap();
        assert_eq!(post_status, status::ACTIVE);
    });
}

#[test]
fn test_stake_zero_fails() {
    with_staking(|_storage, s| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        assert!(s.stake(validator, validator, U256::ZERO).is_err());
    });
}

// ---------------------------------------------------------------------------
// test_unstake
// ---------------------------------------------------------------------------

#[test]
fn test_unstake() {
    with_staking_timed(1_000_000, |storage, s| {
        let validator = address!("0x3333333333333333333333333333333333333333");
        let amount = U256::from(2_000u64);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        s.stake(validator, validator, amount).unwrap();
        s.unstake(validator, U256::from(500u64)).unwrap();

        // Stake reduced
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(1_500u64));
        assert_eq!(s.get_total_staked().unwrap(), U256::from(1_500u64));

        // Queue entry created
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), validator);
        assert_eq!(s.unbonding_amount.read(&0u32).unwrap(), U256::from(500u64));
        // complete_time = 1_000_000 + 3600
        assert_eq!(s.unbonding_complete_time.read(&0u32).unwrap(), 1_003_600u64);
    });
}

#[test]
fn test_unstake_below_min_sets_exiting_status() {
    with_staking_timed(0, |storage, s| {
        let validator = address!("0x4444444444444444444444444444444444444444");
        register_validator(storage.clone(), validator);

        // Stake above min_stake and activate
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        s.stake(validator, validator, U256::from(MIN_STAKE))
            .unwrap();

        // Confirm ACTIVE
        let val_set = ValidatorSet::new(storage.clone());
        assert_eq!(val_set.val_status.read(&validator).unwrap(), status::ACTIVE);

        // Unstake to drop below min_stake
        s.unstake(validator, U256::from(500u64)).unwrap();

        // Should now be EXITING (DKG reshare pending to exclude from consensus)
        let val_set = ValidatorSet::new(storage.clone());
        assert_eq!(
            val_set.val_status.read(&validator).unwrap(),
            status::EXITING
        );
    });
}

#[test]
fn test_unstake_insufficient_fails() {
    with_staking(|storage, s| {
        let validator = address!("0x5555555555555555555555555555555555555555");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 100);
        s.stake(validator, validator, U256::from(100u64)).unwrap();
        assert!(s.unstake(validator, U256::from(200u64)).is_err());
    });
}

// ---------------------------------------------------------------------------
// test_slash_stake
// ---------------------------------------------------------------------------

#[test]
fn test_slash_stake() {
    with_staking(|storage, s| {
        let validator = address!("0x6666666666666666666666666666666666666666");
        let initial = U256::from(1_000u64);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 1_000);
        s.stake(validator, validator, initial).unwrap();
        let slashed = s.slash_stake(validator, 20).unwrap(); // 20%

        // 1000 * 20 / 100 = 200 slashed
        assert_eq!(slashed, U256::from(200u64));
        // 1000 - 200 = 800
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(800u64));
        assert_eq!(s.get_total_staked().unwrap(), U256::from(800u64));
    });
}

#[test]
fn test_slash_stake_100_percent() {
    with_staking(|storage, s| {
        let validator = address!("0x7777777777777777777777777777777777777777");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 500);
        s.stake(validator, validator, U256::from(500u64)).unwrap();
        let slashed = s.slash_stake(validator, 100).unwrap();

        assert_eq!(slashed, U256::from(500u64));
        assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
        assert_eq!(s.get_total_staked().unwrap(), U256::ZERO);
    });
}

#[test]
fn test_slash_above_100_fails() {
    with_staking(|storage, s| {
        let validator = address!("0x8888888888888888888888888888888888888888");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 100);
        s.stake(validator, validator, U256::from(100u64)).unwrap();
        assert!(s.slash_stake(validator, 101).is_err());
    });
}

#[test]
fn test_slash_below_min_stake_transitions_to_exiting() {
    with_staking(|storage, s| {
        let validator = address!("0x9999999999999999999999999999999999999999");
        register_validator(storage.clone(), validator);

        // Stake exactly at min and activate
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        s.stake(validator, validator, U256::from(MIN_STAKE))
            .unwrap();
        let val_set = ValidatorSet::new(storage.clone());
        assert_eq!(val_set.val_status.read(&validator).unwrap(), status::ACTIVE);

        // Slash 50% — new stake = 500, below min_stake (1000)
        // Now auto-transitions ACTIVE → EXITING when stake < min_stake
        s.slash_stake(validator, 50).unwrap();

        // Status transitions to EXITING (stake below min_stake)
        let val_set = ValidatorSet::new(storage.clone());
        assert_eq!(
            val_set.val_status.read(&validator).unwrap(),
            status::EXITING
        );

        // Stake was reduced
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(500u64));

        // Pending set change flagged
        assert!(val_set.pending_set_change.read().unwrap());
    });
}

// ---------------------------------------------------------------------------
// test_claim_unbonded
// ---------------------------------------------------------------------------

#[test]
fn test_claim_unbonded() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 3_600;

    // Setup: stake and unstake before mature time
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        s.stake(validator, validator, U256::from(2_000u64)).unwrap();
        s.unstake(validator, U256::from(500u64)).unwrap();

        // Entry not yet mature — claim should leave it intact
        s.claim_unbonded(validator).unwrap();
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), validator);
    });

    // Advance time past unbonding period and claim
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        s.claim_unbonded(validator).unwrap();

        // Entry should be zeroed out
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_amount.read(&0u32).unwrap(), U256::ZERO);

        // Validator received native tokens back.
        // stake() no longer deducts from caller; validator balance stays at DEFAULT_BALANCE
        // and claim_unbonded adds the 500 back.
        let ctx = storage.clone();
        let expected = DEFAULT_BALANCE + 500;
        assert_eq!(ctx.balance(validator).unwrap(), U256::from(expected));
    });
}

// ---------------------------------------------------------------------------
// test_process_unbonding
// ---------------------------------------------------------------------------

#[test]
fn test_process_unbonding_preserves_claimable() {
    with_staking_timed(0, |storage, s| {
        let v1 = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let v2 = address!("0xcccccccccccccccccccccccccccccccccccccccc");

        // Give both validators enough stake to unstake
        seed_balance(storage.clone(), v1, DEFAULT_BALANCE);
        seed_balance(storage.clone(), v2, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 4_000);
        s.stake(v1, v1, U256::from(2_000u64)).unwrap();
        s.stake(v2, v2, U256::from(2_000u64)).unwrap();

        // Both unstake — both entries land at timestamp 0 + 3600
        s.unstake(v1, U256::from(500u64)).unwrap();
        s.unstake(v2, U256::from(300u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 2);

        // Process at any timestamp — entries are NOT zeroed (only compaction of
        // already-claimed entries happens). Mature entries remain for claim_unbonded.
        s.process_unbonding(100).unwrap();
        assert_eq!(s.unbonding_count.read().unwrap(), 2);

        s.process_unbonding(10_000).unwrap();
        // Entries still present — process_unbonding only compacts zeroed entries,
        // it does NOT zero mature entries. That is claim_unbonded's responsibility.
        assert_eq!(s.unbonding_count.read().unwrap(), 2);
    });
}

#[test]
fn test_process_unbonding_compacts_zeroed() {
    with_staking_timed(0, |storage, s| {
        let v1 = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        seed_balance(storage.clone(), v1, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        s.stake(v1, v1, U256::from(2_000u64)).unwrap();
        s.unstake(v1, U256::from(500u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 1);

        // Manually zero the entry (simulating what claim_unbonded does)
        s.unbonding_validator.write(&0u32, Address::ZERO).unwrap();
        s.unbonding_amount.write(&0u32, U256::ZERO).unwrap();

        // Process should compact the zeroed entry
        s.process_unbonding(10_000).unwrap();
        assert_eq!(s.unbonding_count.read().unwrap(), 0);
    });
}

#[test]
fn test_process_unbonding_hook() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(100).unwrap();

        let validator = address!("0xdddddddddddddddddddddddddddddddddddddddd");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        s.stake(validator, validator, U256::from(2_000u64)).unwrap();
        // At timestamp 0, complete_time = 0 + 100 = 100
        s.unstake(validator, U256::from(200u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });

    // Call hook at timestamp 200 — process_unbonding only compacts zeroed entries,
    // so the mature entry remains (it must be claimed via claim_unbonded).
    StorageHandle::enter(&mut storage, |storage| {
        hooks::process_unbonding(storage.clone(), 200).unwrap();

        let s = Staking::new(storage.clone());
        // Entry still present — not claimed yet
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });
}

// ---------------------------------------------------------------------------
// test_unbonding_full_flow: stake → unstake → advance time → claim → verify balance
// ---------------------------------------------------------------------------

#[test]
fn test_unbonding_full_flow() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 3_600;
    let stake_amount: u64 = 5_000;
    let unstake_amount: u64 = 2_000;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");

    // 1. Stake
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        // A-01: stake() no longer transfers; seed STAKING_ADDRESS to simulate EVM msg.value.
        seed_staking_balance(storage.clone(), stake_amount);
        s.stake(validator, validator, U256::from(stake_amount))
            .unwrap();

        // Verify STAKING_ADDRESS was seeded correctly
        let ctx = storage.clone();
        assert_eq!(
            ctx.balance(STAKING_ADDRESS).unwrap(),
            U256::from(stake_amount),
        );
    });

    // 2. Unstake
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        s.unstake(validator, U256::from(unstake_amount)).unwrap();
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });

    // 3. process_unbonding — mature entry NOT zeroed
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        hooks::process_unbonding(storage.clone(), base_time + unbonding_period + 1).unwrap();
        let s = Staking::new(storage.clone());
        assert_eq!(
            s.unbonding_count.read().unwrap(),
            1,
            "entry must survive process_unbonding"
        );
    });

    // 4. claim_unbonded — funds returned to validator
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        s.claim_unbonded(validator).unwrap();

        // Entry zeroed by claim
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO,);

        // Validator received the unstaked tokens.
        // stake() no longer deducts from caller; validator balance stays at DEFAULT_BALANCE
        // and claim_unbonded adds the unstaked amount back.
        let ctx = storage.clone();
        let expected_balance = DEFAULT_BALANCE + unstake_amount;
        assert_eq!(
            ctx.balance(validator).unwrap(),
            U256::from(expected_balance)
        );

        // Staking contract balance decreased by the claimed amount
        assert_eq!(
            ctx.balance(STAKING_ADDRESS).unwrap(),
            U256::from(stake_amount - unstake_amount),
        );
    });

    // 5. process_unbonding now compacts the zeroed entry
    StorageHandle::enter(&mut storage, |storage| {
        hooks::process_unbonding(storage.clone(), base_time + unbonding_period + 100).unwrap();
        let s = Staking::new(storage.clone());
        assert_eq!(
            s.unbonding_count.read().unwrap(),
            0,
            "zeroed entry should be compacted"
        );
    });
}

// ---------------------------------------------------------------------------
// test_process_unbonding_capped — verify MAX_COMPACTION_PER_BLOCK limit
// ---------------------------------------------------------------------------
#[test]
fn test_process_unbonding_capped() {
    with_staking(|_storage, s| {
        // Create 100 zeroed unbonding entries (simulating previously-claimed entries)
        let total: u32 = 100;
        for i in 0..total {
            s.unbonding_validator.write(&i, Address::ZERO).unwrap();
            s.unbonding_amount.write(&i, U256::ZERO).unwrap();
            s.unbonding_complete_time.write(&i, 0).unwrap();
        }
        s.unbonding_count.write(total).unwrap();

        // First call: compacts at most MAX_COMPACTION_PER_BLOCK (64)
        s.process_unbonding(0).unwrap();
        let count_after_first = s.unbonding_count.read().unwrap();
        // All 100 entries are zeroed, but only 64 compactions allowed per call
        // Since all entries are zero from tail too, compaction just decrements
        assert!(
            count_after_first <= total - Staking::MAX_COMPACTION_PER_BLOCK,
            "should compact at most 64 entries, got count={}",
            count_after_first
        );

        // Second call gets the rest
        s.process_unbonding(0).unwrap();
        assert_eq!(
            s.unbonding_count.read().unwrap(),
            0,
            "all zeroed entries compacted"
        );
    });
}

// ---------------------------------------------------------------------------
// P2-2: Per-validator unbonding linked list tests
// ---------------------------------------------------------------------------

#[test]
fn test_claim_unbonded_linked_list_basic() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Unstake 3 times
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 3_000);
        s.stake(validator, validator, U256::from(3_000u64)).unwrap();
        s.unstake(validator, U256::from(100u64)).unwrap();
        s.unstake(validator, U256::from(200u64)).unwrap();
        s.unstake(validator, U256::from(300u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 3);
        // Head should point to last unstake (prepend: 3→2→1)
        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 3); // stored = idx+1 = 2+1
    });

    // Advance past unbonding period and claim all
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.claim_unbonded(validator).unwrap();

        // All entries zeroed
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_validator.read(&1u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_validator.read(&2u32).unwrap(), Address::ZERO);

        // Linked list head cleared
        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 0);

        // Validator received 100 + 200 + 300 = 600.
        // stake() no longer deducts from caller; validator stays at DEFAULT_BALANCE
        // and claim_unbonded adds 600 back.
        let ctx = storage.clone();
        let expected = DEFAULT_BALANCE + 600;
        assert_eq!(ctx.balance(validator).unwrap(), U256::from(expected));
    });
}

#[test]
fn test_claim_unbonded_partial_maturity() {
    let base_time: u64 = 10_000;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(100).unwrap(); // short period

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 3_000);
        s.stake(validator, validator, U256::from(3_000u64)).unwrap();

        // Entry 0: complete at 10100
        s.unstake(validator, U256::from(100u64)).unwrap();
        s.unstake(validator, U256::from(200u64)).unwrap();
    });

    // Change unbonding period for next unstake — entry 2 will mature much later
    storage.set_timestamp(U256::from(base_time + 50));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(10_000).unwrap(); // long period

        // Entry 2: complete at 10050 + 10000 = 20050
        s.unstake(validator, U256::from(300u64)).unwrap();
    });

    // Advance to 10200 — entries 0,1 mature (10100), entry 2 not (20050)
    storage.set_timestamp(U256::from(10_200));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.claim_unbonded(validator).unwrap();

        // Entries 0,1 zeroed
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_validator.read(&1u32).unwrap(), Address::ZERO);

        // Entry 2 still present
        assert_eq!(s.unbonding_validator.read(&2u32).unwrap(), validator);

        // Linked list head points to entry 2 (stored = 3)
        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 3);

        // Next of entry 2 = 0 (end of list)
        assert_eq!(s.unbonding_next.read(&2u32).unwrap(), 0);

        // Validator received 100 + 200 = 300.
        // stake() no longer deducts from caller; validator stays at DEFAULT_BALANCE
        // and claim_unbonded adds 300 back.
        let ctx = storage.clone();
        let expected = DEFAULT_BALANCE + 300;
        assert_eq!(ctx.balance(validator).unwrap(), U256::from(expected));
    });
}

#[test]
fn test_claim_unbonded_two_validators() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let v1 = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let v2 = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        seed_balance(storage.clone(), v1, DEFAULT_BALANCE);
        seed_balance(storage.clone(), v2, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 6_000);
        s.stake(v1, v1, U256::from(3_000u64)).unwrap();
        s.stake(v2, v2, U256::from(3_000u64)).unwrap();

        // v1 unstakes twice, v2 unstakes once
        s.unstake(v1, U256::from(100u64)).unwrap(); // idx 0
        s.unstake(v2, U256::from(200u64)).unwrap(); // idx 1
        s.unstake(v1, U256::from(300u64)).unwrap(); // idx 2

        // v1 head: 3 (idx 2) → 1 (idx 0) → end
        assert_eq!(s.per_val_unbonding_head.read(&v1).unwrap(), 3);
        // v2 head: 2 (idx 1) → end
        assert_eq!(s.per_val_unbonding_head.read(&v2).unwrap(), 2);
    });

    // Advance past unbonding period
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());

        // Only v1 claims
        s.claim_unbonded(v1).unwrap();

        // v1 entries (0, 2) zeroed
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_validator.read(&2u32).unwrap(), Address::ZERO);
        assert_eq!(s.per_val_unbonding_head.read(&v1).unwrap(), 0);

        // v2 entry (1) still present
        assert_eq!(s.unbonding_validator.read(&1u32).unwrap(), v2);
        assert_eq!(s.per_val_unbonding_head.read(&v2).unwrap(), 2); // stored = 1+1

        // v1 got 100 + 300 = 400.
        // stake() no longer deducts from caller; v1 stays at DEFAULT_BALANCE
        // and claim_unbonded adds 400 back.
        let ctx = storage.clone();
        assert_eq!(ctx.balance(v1).unwrap(), U256::from(DEFAULT_BALANCE + 400));
        // v2 balance unchanged (hasn't claimed); still at DEFAULT_BALANCE since stake() didn't deduct.
        assert_eq!(ctx.balance(v2).unwrap(), U256::from(DEFAULT_BALANCE));
    });
}

#[test]
fn test_process_unbonding_tail_trim() {
    with_staking(|_storage, s| {
        // Create entries: [v1, ZERO, ZERO] — tail trim should remove 2 zeroed tail entries
        let v1 = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        s.unbonding_validator.write(&0u32, v1).unwrap();
        s.unbonding_amount.write(&0u32, U256::from(100u64)).unwrap();
        s.unbonding_complete_time.write(&0u32, 9999u64).unwrap();

        s.unbonding_validator.write(&1u32, Address::ZERO).unwrap();
        s.unbonding_validator.write(&2u32, Address::ZERO).unwrap();
        s.unbonding_count.write(3).unwrap();

        s.process_unbonding(0).unwrap();

        // Only 1 entry remains (the non-zero one)
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
        // Entry 0 still intact
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), v1);
    });
}

#[test]
fn test_process_unbonding_no_trim_when_tail_nonzero() {
    with_staking(|_storage, s| {
        // Create entries: [ZERO, v1] — tail is non-zero, no trim
        let v1 = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        s.unbonding_validator.write(&0u32, Address::ZERO).unwrap();
        s.unbonding_validator.write(&1u32, v1).unwrap();
        s.unbonding_amount.write(&1u32, U256::from(100u64)).unwrap();
        s.unbonding_count.write(2).unwrap();

        s.process_unbonding(0).unwrap();

        // Count unchanged — tail is non-zero
        assert_eq!(s.unbonding_count.read().unwrap(), 2);
    });
}

#[test]
fn test_unstake_prepend_linked_list() {
    with_staking_timed(0, |storage, s| {
        let v = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        seed_balance(storage.clone(), v, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        s.stake(v, v, U256::from(5_000u64)).unwrap();

        s.unstake(v, U256::from(100u64)).unwrap(); // idx 0
        s.unstake(v, U256::from(200u64)).unwrap(); // idx 1
        s.unstake(v, U256::from(300u64)).unwrap(); // idx 2

        // Head = 3 (stored = idx 2 + 1)
        assert_eq!(s.per_val_unbonding_head.read(&v).unwrap(), 3);
        // idx 2 → next = 2 (stored = idx 1 + 1)
        assert_eq!(s.unbonding_next.read(&2u32).unwrap(), 2);
        // idx 1 → next = 1 (stored = idx 0 + 1)
        assert_eq!(s.unbonding_next.read(&1u32).unwrap(), 1);
        // idx 0 → next = 0 (end of list)
        assert_eq!(s.unbonding_next.read(&0u32).unwrap(), 0);
    });
}

// ===========================================================================
// A-04: Slash unbonding entries regression tests
// ===========================================================================

/// A-04: unstake → slash → claim: unbonding amount must be reduced.
#[test]
fn test_slash_reduces_unbonding() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 10_000);
        s.stake(validator, validator, U256::from(10_000u64))
            .unwrap();

        // Unstake 8000 into unbonding
        s.unstake(validator, U256::from(8_000u64)).unwrap();

        // Slash 50% — must hit both active stake (2000) and unbonding (8000)
        let slashed = s.slash_stake(validator, 50).unwrap();

        // Active: 2000 * 50% = 1000 slashed
        // Unbonding: 8000 * 50% = 4000 slashed
        // Total slashed = 5000
        assert_eq!(slashed, U256::from(5_000u64));
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(1_000u64));
        assert_eq!(
            s.unbonding_amount.read(&0u32).unwrap(),
            U256::from(4_000u64)
        );
    });

    // Normal maturity is not enough after slash; slashed entries use the
    // extended withdrawability delay.
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        assert_eq!(ctx.balance(validator).unwrap(), U256::from(DEFAULT_BALANCE));
    });

    // Claim after slashed withdrawability delay — should receive reduced amount.
    storage.set_timestamp(U256::from(base_time + (unbonding_period * 2) + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        assert_eq!(
            ctx.balance(validator).unwrap(),
            U256::from(DEFAULT_BALANCE + 4_000)
        );
    });
}

/// A-04: 100% slash zeroes all unbonding entries.
#[test]
fn test_slash_100_zeroes_unbonding() {
    with_staking_timed(0, |storage, s| {
        let validator = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        s.stake(validator, validator, U256::from(5_000u64)).unwrap();

        s.unstake(validator, U256::from(1_000u64)).unwrap();
        s.unstake(validator, U256::from(2_000u64)).unwrap();

        let slashed = s.slash_stake(validator, 100).unwrap();

        // Active: 2000 * 100% = 2000
        // Unbonding[0]: 1000 * 100% = 1000
        // Unbonding[1]: 2000 * 100% = 2000
        // Total = 5000
        assert_eq!(slashed, U256::from(5_000u64));
        assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
        assert_eq!(s.unbonding_amount.read(&0u32).unwrap(), U256::ZERO);
        assert_eq!(s.unbonding_amount.read(&1u32).unwrap(), U256::ZERO);
    });
}

// ===========================================================================
// A-05: Balance invariant after slash
// ===========================================================================

/// A-05: After slash, STAKING_ADDRESS balance == remaining stake + remaining unbonding.
#[test]
fn test_slash_balance_invariant() {
    with_staking(|storage, s| {
        let validator = address!("0xcccccccccccccccccccccccccccccccccccccccc");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 10_000);
        s.stake(validator, validator, U256::from(10_000u64))
            .unwrap();

        s.unstake(validator, U256::from(3_000u64)).unwrap();

        // Slash 20%
        s.slash_stake(validator, 20).unwrap();

        let remaining_stake = s.get_stake(validator).unwrap();
        let remaining_unbonding = s.unbonding_amount.read(&0u32).unwrap();
        let staking_balance = storage.balance(STAKING_ADDRESS).unwrap();

        // balance == stake + unbonding (no orphaned tokens)
        assert_eq!(
            staking_balance,
            remaining_stake + remaining_unbonding,
            "STAKING_ADDRESS balance must equal stake + unbonding after slash"
        );
    });
}

// ===========================================================================
// A-43: Self-stake only — unstake/claim rights remain with the staker
// ===========================================================================

/// A-43: Self-staker can unstake and claim their own funds.
#[test]
fn test_self_staker_can_unstake_and_claim() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        s.stake(validator, validator, U256::from(5_000u64)).unwrap();
        s.unstake(validator, U256::from(2_000u64)).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), U256::from(3_000u64));
    });

    // Advance past unbonding and claim
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(U256::from(MIN_STAKE)).unwrap();
        s.config_unbonding_period.write(unbonding_period).unwrap();

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        // Validator received 2000 back
        assert_eq!(
            ctx.balance(validator).unwrap(),
            U256::from(DEFAULT_BALANCE + 2_000)
        );
    });
}
