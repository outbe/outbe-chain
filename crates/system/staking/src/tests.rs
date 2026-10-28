use alloy_primitives::{address, Address, Log, B256, U256, U512};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_whole_coen_to_native;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::test_support::test_lifecycle_of;
use outbe_validatorset::ValidatorLifecycle;

use crate::contract::Staking;
use crate::hooks;
use crate::precompile::{dispatch, IStaking};

const CHAIN_ID: u64 = 1;
pub(crate) const MIN_STAKE: u64 = 1_000;

/// Default large balance seeded to callers so transfer_balance succeeds.
const DEFAULT_BALANCE: u64 = 1_000_000;

#[test]
fn stake_addition_overflows_leave_accounting_unchanged() {
    let validator = Address::repeat_byte(0x31);
    // This test uses inconsistent storage to isolate each failure of an accounting invariant.
    for (stake, total, expected) in [
        (
            U256::MAX,
            U256::MAX,
            IStaking::StakeAccountingOverflow { validator }.abi_encode(),
        ),
        (
            U256::ZERO,
            U256::MAX,
            IStaking::TotalStakeAccountingOverflow {}.abi_encode(),
        ),
    ] {
        with_staking(|storage, s| {
            register_validator(storage.clone(), validator);
            s.stake_amount.write(&validator, stake).unwrap();
            s.total_staked.write(total).unwrap();
            let before = ValidatorSet::new(storage.clone())
                .validator_state(validator)
                .unwrap();
            let error = s.topup(validator, validator, U256::from(1)).unwrap_err();
            assert!(
                matches!(error, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected)
            );
            assert_eq!(s.get_stake(validator).unwrap(), stake);
            assert_eq!(s.get_total_staked().unwrap(), total);
            assert_eq!(
                ValidatorSet::new(storage)
                    .validator_state(validator)
                    .unwrap(),
                before
            );
        });
    }
}

#[test]
fn aggregate_underflow_precedes_unstake_and_slash_writes() {
    let validator = Address::repeat_byte(0x32);
    for slash in [false, true] {
        with_staking(|storage, s| {
            seed_staking_balance(storage.clone(), MIN_STAKE);
            stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
            // Corrupt only the aggregate total to test an accounting invariant.
            // Valid state cannot contain this inconsistency.
            s.total_staked.write(U256::ZERO).unwrap();
            let before = ValidatorSet::new(storage.clone())
                .validator_state(validator)
                .unwrap();
            let error = if slash {
                s.slash_stake(validator, 100).unwrap_err()
            } else {
                s.unstake(validator, U256::from(1)).unwrap_err()
            };
            let PrecompileError::FatalWithCause(cause) = error else {
                panic!("expected fatal stake accounting corruption, got {error:?}");
            };
            assert!(
                matches!(cause.downcast_ref::<crate::errors::StakingError>(),
                Some(crate::errors::StakingError::StakeAccountingUnderflow { validator: actual })
                    if *actual == validator)
            );
            assert_eq!(s.get_stake(validator).unwrap(), U256::from(MIN_STAKE));
            assert_eq!(s.get_total_staked().unwrap(), U256::ZERO);
            assert_eq!(s.unbonding_count.read().unwrap(), 0);
            assert_eq!(
                storage.balance(STAKING_ADDRESS).unwrap(),
                U256::from(MIN_STAKE)
            );
            assert_eq!(
                ValidatorSet::new(storage)
                    .validator_state(validator)
                    .unwrap(),
                before
            );
        });
    }
}

#[test]
fn concentration_uses_wide_products_at_cap_boundaries() {
    let other = Address::repeat_byte(0x33);
    let validator = Address::repeat_byte(0x34);
    let half = (U256::MAX / U256::from(4)) * U256::from(2);
    for amount in [half - U256::from(1), half, half + U256::from(1)] {
        with_staking(|storage, s| {
            seed_staking_balance_u256(storage.clone(), half + amount);
            s.config_max_stake_percent.write(50).unwrap();
            // The first staker remains exempt even though it owns 100% of stake.
            stake_registered(storage.clone(), s, other, half).unwrap();
            assert!(s.topup(other, other, U256::from(1)).is_err());
            assert_eq!(s.get_total_staked().unwrap(), half);
            register_validator(storage.clone(), validator);
            let result = s.topup(validator, validator, amount);
            if amount > half {
                assert!(
                    matches!(result.unwrap_err(), PrecompileError::RevertBytes(bytes)
                    if bytes.as_ref() == IStaking::StakeExceedsMaxStakePercent {}.abi_encode())
                );
                assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
                assert_eq!(s.get_total_staked().unwrap(), half);
            } else {
                result.unwrap();
                assert_eq!(s.get_stake(validator).unwrap(), amount);
                assert_eq!(s.get_total_staked().unwrap(), half + amount);
                assert_eq!(
                    ValidatorSet::new(storage)
                        .validator_state(validator)
                        .unwrap()
                        .bonded_stake(),
                    amount
                );
            }
        });
    }
    for max_pct in [0, 100, 101, u64::MAX] {
        with_staking(|storage, s| {
            s.config_max_stake_percent.write(max_pct).unwrap();
            stake_registered(storage.clone(), s, validator, half).unwrap();
            s.topup(validator, validator, U256::from(1)).unwrap();
            assert_eq!(s.get_total_staked().unwrap(), half + U256::from(1));
        });
    }
}

#[test]
fn slash_large_and_remainder_amounts_match_wide_oracle_and_native_burn() {
    let validator = Address::repeat_byte(0x35);
    for (amount, unbonding, percent) in [
        (U256::MAX, U256::ZERO, 100),
        (U256::MAX, U256::from(199), 37),
        (U256::MAX, U256::MAX, 100),
        (U256::MAX, U256::MAX, 37),
        (U256::from(199), U256::from(99), 1),
        (U256::from(1), U256::ZERO, 0),
    ] {
        with_staking(|storage, s| {
            s.config_min_stake.write(U256::from(1)).unwrap();
            seed_staking_balance_u256(storage.clone(), amount);
            stake_registered(storage.clone(), s, validator, amount).unwrap();
            if !unbonding.is_zero() {
                s.unstake(validator, unbonding).unwrap();
            }
            let bonded = amount - unbonding;
            let oracle = |value: U256| {
                (U512::from(value) * U512::from(percent) / U512::from(100)).to::<U256>()
            };
            let expected = oracle(bonded) + oracle(unbonding);
            assert_eq!(s.slash_stake(validator, percent).unwrap(), expected);
            assert_eq!(s.get_stake(validator).unwrap(), bonded - oracle(bonded));
            assert_eq!(s.get_total_staked().unwrap(), bonded - oracle(bonded));
            if !unbonding.is_zero() {
                assert_eq!(
                    s.unbonding_amount.read(&0).unwrap(),
                    unbonding - oracle(unbonding)
                );
            }
            assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), amount - expected);
            assert_eq!(
                ValidatorSet::new(storage)
                    .validator_state(validator)
                    .unwrap()
                    .bonded_stake(),
                bonded - oracle(bonded)
            );
        });
    }
}

#[test]
fn late_queue_slash_overflow_rolls_back_checkpointed_cross_module_effects() {
    let validator = Address::repeat_byte(0x36);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), 3600);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), &mut s, validator, U256::from(MIN_STAKE)).unwrap();
        ValidatorSet::new(storage.clone())
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();
        // This test violates an invariant: queue liabilities exceed the maximum native supply.
        // Prepend the small entry so the slash changes it before the later MAX entry fails.
        s.enqueue_unbonding(validator, U256::MAX, 1).unwrap();
        s.enqueue_unbonding(validator, U256::from(1), 2).unwrap();
    });
    let events_before = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        let mut validators = ValidatorSet::new(storage.clone());
        let before = validators.validator_state(validator).unwrap();
        let result = storage.with_checkpoint(|| {
            // Jail emits an event and changes the lifecycle before the slash starts.
            validators.jail_validator(validator)?;
            s.slash_stake(validator, 100)
        });
        let error = result.unwrap_err();
        assert!(matches!(error, PrecompileError::RevertBytes(bytes)
            if bytes.as_ref() == IStaking::SlashAmountOverflow {}.abi_encode()));
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(MIN_STAKE));
        assert_eq!(s.get_total_staked().unwrap(), U256::from(MIN_STAKE));
        assert_eq!(s.unbonding_amount.read(&0).unwrap(), U256::MAX);
        assert_eq!(s.unbonding_amount.read(&1).unwrap(), U256::from(1));
        assert_eq!(s.unbonding_complete_time.read(&0).unwrap(), 1);
        assert_eq!(s.unbonding_complete_time.read(&1).unwrap(), 2);
        assert_eq!(s.unbonding_count.read().unwrap(), 2);
        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 2);
        assert_eq!(s.unbonding_next.read(&1).unwrap(), 1);
        assert_eq!(
            storage.balance(STAKING_ADDRESS).unwrap(),
            U256::from(MIN_STAKE)
        );
        assert_eq!(validators.validator_state(validator).unwrap(), before);
    });
    assert_eq!(provider.get_ordered_events(), events_before);
}

#[test]
fn slash_timestamp_overflow_leaves_stake_balance_queue_and_events_unchanged() {
    let validator = address!("0x1111111111111111111111111111111111111111");
    let amount = U256::from(MIN_STAKE);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = staking_with_unbonding_period(storage.clone(), 3600);
        staking.stake_amount.write(&validator, amount).unwrap();
        staking.total_staked.write(amount).unwrap();
        storage.set_balance(STAKING_ADDRESS, amount).unwrap();
        storage
            .set_block_timestamp(U256::from(u64::MAX) + U256::from(1))
            .unwrap();
        assert!(matches!(
            staking.slash_stake(validator, 10),
            Err(outbe_primitives::error::PrecompileError::Revert(message))
                if message == "timestamp exceeds u64"
        ));
        assert_eq!(staking.get_stake(validator).unwrap(), amount);
        assert_eq!(staking.get_total_staked().unwrap(), amount);
        assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), amount);
        assert_eq!(staking.unbonding_count.read().unwrap(), 0);
        assert_eq!(staking.per_val_unbonding_head.read(&validator).unwrap(), 0);
    });
    assert!(provider.get_ordered_events().is_empty());
}

pub(crate) fn with_staking<R>(f: impl FnOnce(StorageHandle, &mut Staking) -> R) -> R {
    with_staking_timed(0, f)
}

fn with_staking_timed<R>(timestamp: u64, f: impl FnOnce(StorageHandle, &mut Staking) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    // Height zero is the persisted "not set" sentinel for lifecycle heights.
    // Ordinary staking transactions execute only after genesis.
    storage.set_block_number(1);
    storage.set_timestamp(U256::from(timestamp));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), 3600);
        f(storage, &mut s)
    })
}

/// Staking with the test minimum stake.
fn staking_with_min_stake(storage: StorageHandle<'_>) -> Staking<'_> {
    let staking = Staking::new(storage);
    staking
        .config_min_stake
        .write(U256::from(MIN_STAKE))
        .expect("write min_stake");
    staking
}

/// Staking with the test minimum stake, then `unbonding_period`.
fn staking_with_unbonding_period(storage: StorageHandle<'_>, unbonding_period: u64) -> Staking<'_> {
    let staking = staking_with_min_stake(storage);
    staking
        .config_unbonding_period
        .write(unbonding_period)
        .expect("write unbonding_period");
    staking
}

/// Seed a caller's native balance so transfer_balance in stake() succeeds.
pub(crate) fn seed_balance(storage: StorageHandle, addr: Address, amount: u64) {
    let ctx = storage.clone();
    ctx.set_balance(addr, U256::from(amount)).unwrap();
}

/// Registers a validator in ValidatorSet so cross-calls work correctly.
/// Uses the explicit test-only bootstrap seam. Production registration requires PoP.
pub(crate) fn register_validator(storage: StorageHandle, validator: Address) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    let mut val_set = ValidatorSet::new(storage.clone());
    val_set
        .test_configure_registry(owner)
        .expect("configure registry");
    let mut consensus_pubkey = [0u8; 48];
    consensus_pubkey[..20].copy_from_slice(validator.as_slice());
    val_set
        .test_register_validator_without_pop(validator, &consensus_pubkey)
        .expect("register_validator");
}

pub(crate) fn test_bls_registration_material(
    validator: Address,
    seed: u8,
) -> ([u8; 48], B256, [u8; 96]) {
    use blst::min_pk::SecretKey;
    use outbe_primitives::validators::{
        validator_registration_message, VALIDATOR_REGISTRATION_DST,
    };

    let mut ikm = [seed; 32];
    ikm[..20].copy_from_slice(validator.as_slice());
    let sk = SecretKey::key_gen(&ikm, &[]).expect("BLS keygen");
    let pk: [u8; 48] = sk.sk_to_pk().to_bytes();
    let node_id = alloy_primitives::keccak256(validator.as_slice());
    let msg = validator_registration_message(CHAIN_ID, validator, node_id);
    let sig: [u8; 96] = sk.sign(&msg, VALIDATOR_REGISTRATION_DST, &[]).to_bytes();
    (pk, node_id, sig)
}

pub(crate) fn stake_registered(
    storage: StorageHandle,
    staking: &mut Staking<'_>,
    validator: Address,
    amount: U256,
) -> outbe_primitives::error::Result<()> {
    let val_set = ValidatorSet::new(storage.clone());
    if !val_set.is_validator(validator)? {
        register_validator(storage, validator);
    }
    staking.topup(validator, validator, amount)
}

/// Seeds STAKING_ADDRESS with balance (simulating EVM-level msg.value transfer).
/// stake() no longer transfers. In production, the EVM does it.
pub(crate) fn seed_staking_balance(storage: StorageHandle, amount: u64) {
    seed_staking_balance_u256(storage, U256::from(amount));
}

pub(crate) fn seed_staking_balance_u256(storage: StorageHandle, amount: U256) {
    let ctx = storage.clone();
    let current = ctx.balance(STAKING_ADDRESS).unwrap();
    ctx.set_balance(STAKING_ADDRESS, current + amount).unwrap();
}

// ---------------------------------------------------------------------------
// test_stake
// ---------------------------------------------------------------------------

#[test]
fn test_stake() {
    with_staking(|storage, s| {
        // Self-stake only (caller == validator)
        let validator = address!("0x1111111111111111111111111111111111111111");
        let amount = U256::from(500u64);

        // stake() does not transfer funds. In production, the EVM does it.
        // Seed STAKING_ADDRESS to simulate EVM msg.value transfer.
        seed_staking_balance(storage.clone(), 500);
        stake_registered(storage.clone(), s, validator, amount).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), amount);
        assert_eq!(s.get_total_staked().unwrap(), amount);
    });
}

#[test]
fn test_stake_with_eighteen_decimal_native_coen_fixture() {
    with_staking(|storage, s| {
        let validator = address!("0x1212121212121212121212121212121212121212");
        let amount = checked_whole_coen_to_native(U256::from(100_000u64)).unwrap();
        let (pk, node_id, sig) = test_bls_registration_material(validator, 0x12);

        ValidatorSet::new(storage.clone())
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();
        s.config_min_stake.write(amount).unwrap();
        seed_staking_balance_u256(storage.clone(), amount);
        s.stake(validator, validator, amount, (&pk, node_id, &sig), (0, &[]))
            .unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), amount);
        assert_eq!(s.get_total_staked().unwrap(), amount);
        assert!(matches!(
            test_lifecycle_of(storage, validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
    });
}

#[test]
fn test_stake_third_party_rejected() {
    with_staking(|_storage, s| {
        // Staking rejects third-party stake and topup calls.
        let validator = address!("0x1111111111111111111111111111111111111111");
        let caller = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let (pk, node_id, sig) = test_bls_registration_material(validator, 0x11);
        assert!(s
            .stake(
                caller,
                validator,
                U256::from(MIN_STAKE),
                (&pk, node_id, &sig),
                (0, &[])
            )
            .is_err());
        assert!(s.topup(caller, validator, U256::from(500u64)).is_err());
    });
}

#[test]
fn test_stake_accumulates() {
    with_staking(|storage, s| {
        let validator = address!("0x1111111111111111111111111111111111111111");

        seed_staking_balance(storage.clone(), 1_000);
        stake_registered(storage.clone(), s, validator, U256::from(300u64)).unwrap();
        stake_registered(storage.clone(), s, validator, U256::from(700u64)).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), U256::from(1_000u64));
        assert_eq!(s.get_total_staked().unwrap(), U256::from(1_000u64));
    });
}

#[test]
fn test_stake_marks_registered_validator_pending() {
    with_staking(|storage, s| {
        let validator = address!("0x2222222222222222222222222222222222222222");
        let (pk, node_id, sig) = test_bls_registration_material(validator, 0x22);
        ValidatorSet::new(storage.clone())
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();

        // Initial status is Absent.
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::Absent
        ));

        // Stake enough to meet min_stake -> registers and enters WaitingForReadiness (PENDING)
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        s.stake(
            validator,
            validator,
            U256::from(MIN_STAKE),
            (&pk, node_id, &sig),
            (0, &[]),
        )
        .unwrap();

        // PoS: staking at or above min_stake registers and marks the validator PENDING
        // (admitted, syncing, not yet voting). The DKG reshare promotes PENDING->ACTIVE
        // once it gets a share. Registration sets pending_set_change so consensus schedules it.
        let val_set = ValidatorSet::new(storage.clone());
        let lifecycle = val_set.validator_lifecycle(validator).unwrap();
        assert!(matches!(
            &lifecycle,
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        assert!(val_set.has_pending_set_change().unwrap());
        assert!(!lifecycle.has_bls_share());
    });
}

#[test]
fn test_stake_zero_fails() {
    with_staking(|_storage, s| {
        let validator = address!("0x1111111111111111111111111111111111111111");
        let (pk, node_id, sig) = test_bls_registration_material(validator, 0x11);
        assert!(s
            .stake(
                validator,
                validator,
                U256::ZERO,
                (&pk, node_id, &sig),
                (0, &[])
            )
            .is_err());
        assert!(s.topup(validator, validator, U256::ZERO).is_err());
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
        stake_registered(storage.clone(), s, validator, amount).unwrap();
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
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();

        // Stake marks PENDING. Simulate the reshare promotion to ACTIVE so this
        // test exercises the unstake-below-min ACTIVE->EXITING path.
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .promote_pending_validator_for_test(validator)
            .unwrap();

        // Full unstake from ACTIVE transitions to EXITING (DKG reshare pending to exclude from consensus)
        s.unstake(validator, U256::from(MIN_STAKE)).unwrap();

        // Should now be EXITING (DKG reshare pending to exclude from consensus)
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));
    });
}

#[test]
fn test_unstake_below_min_rejected_while_full_unstake_moves_pending_to_unbonding() {
    with_staking_timed(0, |storage, s| {
        let validator = address!("0x4A44444444444444444444444444444444444444");
        register_validator(storage.clone(), validator);
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        // PENDING joiner (not yet activated) unstaking to a non-zero remainder below min_stake
        // reverts with BondedStakeBelowMinimum. A full unstake (remainder == 0) moves to Unbonding.
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        assert!(s.unstake(validator, U256::from(500u64)).is_err());
        s.unstake(validator, U256::from(MIN_STAKE)).unwrap();
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));
    });
}
#[test]
fn test_unstake_from_jailed_goes_exiting() {
    // Leave-from-jail: unstaking the full stake from JAILED routes to EXITING, then
    // the normal EXITING -> UNBONDING -> INACTIVE drain runs.
    with_staking_timed(0, |storage, s| {
        let validator = address!("0x4B44444444444444444444444444444444444444");
        register_validator(storage.clone(), validator);
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();
        val_set.jail_validator(validator).unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));

        s.unstake(validator, U256::from(MIN_STAKE)).unwrap();
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));
    });
}

#[test]
fn test_unjail_requires_min_stake_and_explicit_tx() {
    // unjailValidator needs stake >= min_stake AND is always explicit: a top-up
    // alone does NOT auto-unjail.
    with_staking_timed(0, |storage, s| {
        let validator = address!("0x4D44444444444444444444444444444444444444");
        register_validator(storage.clone(), validator);
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .activate_validator_via_boundary_for_test(validator)
            .unwrap();
        val_set.jail_validator(validator).unwrap();

        // Exclude the jailed validator at a validated boundary and slash its
        // remaining bonded stake so the test exercises the explicit unjail stake check.
        val_set
            .test_activate_validated_boundary_set(&[], B256::ZERO, 1)
            .unwrap();
        s.slash_stake(validator, 100).unwrap();

        // No stake yet -> unjail rejected (needs >= min_stake).
        assert!(
            s.unjail_validator(validator).is_err(),
            "unjail must require stake >= min_stake"
        );

        // Top up to min_stake. This does NOT change the JAILED status by itself.
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        assert!(
            matches!(
                test_lifecycle_of(storage.clone(), validator).unwrap(),
                ValidatorLifecycle::Jail(_)
            ),
            "a stake top-up alone must NOT unjail"
        );

        // Explicit unjail now succeeds -> PENDING.
        s.unjail_validator(validator).unwrap();
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
    });
}

#[test]
fn test_unstake_insufficient_fails() {
    with_staking(|storage, s| {
        let validator = address!("0x5555555555555555555555555555555555555555");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 100);
        stake_registered(storage.clone(), s, validator, U256::from(100u64)).unwrap();
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
        stake_registered(storage.clone(), s, validator, initial).unwrap();
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
        stake_registered(storage.clone(), s, validator, U256::from(500u64)).unwrap();
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
        stake_registered(storage.clone(), s, validator, U256::from(100u64)).unwrap();
        assert!(s.slash_stake(validator, 101).is_err());
    });
}

#[test]
fn test_slash_below_min_stake_transitions_to_exiting() {
    with_staking(|storage, s| {
        let validator = address!("0x9999999999999999999999999999999999999999");
        register_validator(storage.clone(), validator);

        // Stake exactly at min -> PENDING, then simulate a DKG reshare promotion to
        // ACTIVE via manual activation (the slash-below-min path under test demotes a
        // consensus-ACTIVE validator to EXITING).
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .promote_pending_validator_for_test(validator)
            .unwrap();

        // Slash 50% - new stake = 500, below min_stake (1000)
        // Now auto-transitions ACTIVE -> EXITING when stake < min_stake
        s.slash_stake(validator, 50).unwrap();

        // Status transitions to EXITING (stake below min_stake)
        let val_set = ValidatorSet::new(storage.clone());
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));

        // Stake was reduced
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(500u64));

        // Pending set change flagged
        assert!(val_set.has_pending_set_change().unwrap());
    });
}

#[test]
fn test_slash_below_min_stake_demotes_joining_to_waiting_for_readiness() {
    with_staking(|storage, s| {
        let validator = address!("0x9A99999999999999999999999999999999999999");
        register_validator(storage.clone(), validator);

        // Stake -> PENDING, then confirm readiness -> Joining.
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), s, validator, U256::from(MIN_STAKE)).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set.confirm_validator_ready(validator).unwrap();
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::Joining(_)
        ));

        // Slash below min before activation -> demote Joining to WaitingForReadiness so the next
        // reshare target does not select an under-staked joiner.
        s.slash_stake(validator, 50).unwrap();
        let val_set = ValidatorSet::new(storage.clone());
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        assert!(val_set.has_pending_set_change().unwrap());
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
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(2_000u64)).unwrap();
        s.unstake(validator, U256::from(500u64)).unwrap();

        // Entry not yet mature - claim should leave it intact
        s.claim_unbonded(validator).unwrap();
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), validator);
    });

    // Advance time past unbonding period and claim
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        s.claim_unbonded(validator).unwrap();

        // Entry should be zeroed
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO);
        assert_eq!(s.unbonding_amount.read(&0u32).unwrap(), U256::ZERO);

        // Validator received native tokens back.
        // stake() no longer deducts from caller. Validator balance stays at DEFAULT_BALANCE
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
        stake_registered(storage.clone(), s, v1, U256::from(2_000u64)).unwrap();
        stake_registered(storage.clone(), s, v2, U256::from(2_000u64)).unwrap();

        // Both unstake - both entries land at timestamp 0 + 3600
        s.unstake(v1, U256::from(500u64)).unwrap();
        s.unstake(v2, U256::from(300u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 2);

        // Process at any timestamp. process_unbonding does NOT zero entries (it only
        // compacts already-claimed entries). Mature entries remain for claim_unbonded.
        s.process_unbonding(100).unwrap();
        assert_eq!(s.unbonding_count.read().unwrap(), 2);

        s.process_unbonding(10_000).unwrap();
        // Entries still present. process_unbonding only compacts zeroed entries.
        // It does NOT zero mature entries. That is claim_unbonded's responsibility.
        assert_eq!(s.unbonding_count.read().unwrap(), 2);
    });
}

#[test]
fn test_process_unbonding_compacts_zeroed() {
    with_staking_timed(0, |storage, s| {
        let v1 = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        seed_balance(storage.clone(), v1, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), s, v1, U256::from(2_000u64)).unwrap();
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
        let mut s = staking_with_unbonding_period(storage.clone(), 100);

        let validator = address!("0xdddddddddddddddddddddddddddddddddddddddd");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(2_000u64)).unwrap();
        // At timestamp 0, complete_time = 0 + 100 = 100
        s.unstake(validator, U256::from(200u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });

    // Call hook at timestamp 200. process_unbonding only compacts zeroed entries,
    // so the mature entry remains (it must be claimed via claim_unbonded).
    StorageHandle::enter(&mut storage, |storage| {
        hooks::process_unbonding(storage.clone(), 200).unwrap();

        let s = Staking::new(storage.clone());
        // Entry still present - not claimed yet
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });
}

// ---------------------------------------------------------------------------
// test_unbonding_full_flow: stake -> unstake -> advance time -> claim -> verify balance
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
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        // stake() no longer transfers; seed STAKING_ADDRESS to simulate EVM msg.value.
        seed_staking_balance(storage.clone(), stake_amount);
        stake_registered(storage.clone(), &mut s, validator, U256::from(stake_amount)).unwrap();

        // Verify STAKING_ADDRESS was seeded correctly
        let ctx = storage.clone();
        assert_eq!(
            ctx.balance(STAKING_ADDRESS).unwrap(),
            U256::from(stake_amount),
        );
    });

    // 2. Unstake
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        s.unstake(validator, U256::from(unstake_amount)).unwrap();
        assert_eq!(s.unbonding_count.read().unwrap(), 1);
    });

    // 3. process_unbonding - mature entry NOT zeroed
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

    // 4. claim_unbonded - funds returned to validator
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        s.claim_unbonded(validator).unwrap();

        // Entry zeroed by claim
        assert_eq!(s.unbonding_validator.read(&0u32).unwrap(), Address::ZERO,);

        // Validator received the unstaked tokens.
        // stake() no longer deducts from caller. Validator balance stays at DEFAULT_BALANCE
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
// test_process_unbonding_capped - verify MAX_COMPACTION_PER_BLOCK limit
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
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 3_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(3_000u64)).unwrap();
        s.unstake(validator, U256::from(100u64)).unwrap();
        s.unstake(validator, U256::from(200u64)).unwrap();
        s.unstake(validator, U256::from(300u64)).unwrap();

        assert_eq!(s.unbonding_count.read().unwrap(), 3);
        // Head should point to last unstake (prepend: 3->2->1)
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
        // stake() no longer deducts from caller. Validator stays at DEFAULT_BALANCE
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
        let mut s = staking_with_unbonding_period(storage.clone(), 100); // short period

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 3_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(3_000u64)).unwrap();

        // Entry 0: complete at 10100
        s.unstake(validator, U256::from(100u64)).unwrap();
        s.unstake(validator, U256::from(200u64)).unwrap();
    });

    // Change unbonding period for next unstake - entry 2 will mature much later
    storage.set_timestamp(U256::from(base_time + 50));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), 10_000); // long period

        // Entry 2: complete at 10050 + 10000 = 20050
        s.unstake(validator, U256::from(300u64)).unwrap();
    });

    // Advance to 10200 - entries 0,1 mature (10100), entry 2 not (20050)
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
        // stake() no longer deducts from caller. Validator stays at DEFAULT_BALANCE
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
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        seed_balance(storage.clone(), v1, DEFAULT_BALANCE);
        seed_balance(storage.clone(), v2, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 6_000);
        stake_registered(storage.clone(), &mut s, v1, U256::from(3_000u64)).unwrap();
        stake_registered(storage.clone(), &mut s, v2, U256::from(3_000u64)).unwrap();

        // v1 unstakes twice, v2 unstakes once
        s.unstake(v1, U256::from(100u64)).unwrap(); // idx 0
        s.unstake(v2, U256::from(200u64)).unwrap(); // idx 1
        s.unstake(v1, U256::from(300u64)).unwrap(); // idx 2

        // v1 head: 3 (idx 2) -> 1 (idx 0) -> end
        assert_eq!(s.per_val_unbonding_head.read(&v1).unwrap(), 3);
        // v2 head: 2 (idx 1) -> end
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
        // stake() no longer deducts from caller. v1 stays at DEFAULT_BALANCE
        // and claim_unbonded adds 400 back.
        let ctx = storage.clone();
        assert_eq!(ctx.balance(v1).unwrap(), U256::from(DEFAULT_BALANCE + 400));
        // v2 balance unchanged (v2 did not claim). It stays at DEFAULT_BALANCE because
        // stake() did not deduct.
        assert_eq!(ctx.balance(v2).unwrap(), U256::from(DEFAULT_BALANCE));
    });
}

#[test]
fn test_process_unbonding_tail_trim() {
    with_staking(|_storage, s| {
        // Create entries: [v1, ZERO, ZERO] - tail trim should remove 2 zeroed tail entries
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
        // Create entries: [ZERO, v1] - tail is non-zero, no trim
        let v1 = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        s.unbonding_validator.write(&0u32, Address::ZERO).unwrap();
        s.unbonding_validator.write(&1u32, v1).unwrap();
        s.unbonding_amount.write(&1u32, U256::from(100u64)).unwrap();
        s.unbonding_count.write(2).unwrap();

        s.process_unbonding(0).unwrap();

        // Count unchanged - tail is non-zero
        assert_eq!(s.unbonding_count.read().unwrap(), 2);
    });
}

#[test]
fn test_unstake_prepend_linked_list() {
    with_staking_timed(0, |storage, s| {
        let v = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        seed_balance(storage.clone(), v, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        stake_registered(storage.clone(), s, v, U256::from(5_000u64)).unwrap();

        s.unstake(v, U256::from(100u64)).unwrap(); // idx 0
        s.unstake(v, U256::from(200u64)).unwrap(); // idx 1
        s.unstake(v, U256::from(300u64)).unwrap(); // idx 2

        // Head = 3 (stored = idx 2 + 1)
        assert_eq!(s.per_val_unbonding_head.read(&v).unwrap(), 3);
        // idx 2 -> next = 2 (stored = idx 1 + 1)
        assert_eq!(s.unbonding_next.read(&2u32).unwrap(), 2);
        // idx 1 -> next = 1 (stored = idx 0 + 1)
        assert_eq!(s.unbonding_next.read(&1u32).unwrap(), 1);
        // idx 0 -> next = 0 (end of list)
        assert_eq!(s.unbonding_next.read(&0u32).unwrap(), 0);
    });
}

// ===========================================================================
// Slash unbonding entries regression tests
// ===========================================================================

/// unstake -> slash -> claim: unbonding amount must be reduced.
#[test]
fn test_slash_reduces_unbonding() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 10_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(10_000u64)).unwrap();

        // Unstake 8000 into unbonding
        s.unstake(validator, U256::from(8_000u64)).unwrap();

        // Slash 50% - must hit both active stake (2000) and unbonding (8000)
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

    // Normal maturity is not enough after slash. Slashed entries use the
    // extended withdrawability delay.
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        assert_eq!(ctx.balance(validator).unwrap(), U256::from(DEFAULT_BALANCE));
    });

    // Claim after slashed withdrawability delay - should receive reduced amount.
    storage.set_timestamp(U256::from(base_time + (unbonding_period * 2) + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        assert_eq!(
            ctx.balance(validator).unwrap(),
            U256::from(DEFAULT_BALANCE + 4_000)
        );
    });
}

/// 100% slash zeroes all unbonding entries.
#[test]
fn test_slash_100_zeroes_unbonding() {
    with_staking_timed(0, |storage, s| {
        let validator = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        stake_registered(storage.clone(), s, validator, U256::from(5_000u64)).unwrap();

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
// Balance invariant after slash
// ===========================================================================

/// After slash, STAKING_ADDRESS balance == remaining stake + remaining unbonding.
#[test]
fn test_slash_balance_invariant() {
    with_staking(|storage, s| {
        let validator = address!("0xcccccccccccccccccccccccccccccccccccccccc");
        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 10_000);
        stake_registered(storage.clone(), s, validator, U256::from(10_000u64)).unwrap();

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
// Self-stake only - unstake/claim rights remain with the staker
// ===========================================================================

/// Self-staker can unstake and claim their own funds.
#[test]
fn test_self_staker_can_unstake_and_claim() {
    let base_time: u64 = 10_000;
    let unbonding_period: u64 = 100;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(base_time));

    let validator = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");

    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), 5_000);
        stake_registered(storage.clone(), &mut s, validator, U256::from(5_000u64)).unwrap();
        s.unstake(validator, U256::from(2_000u64)).unwrap();

        assert_eq!(s.get_stake(validator).unwrap(), U256::from(3_000u64));
    });

    // Advance past unbonding and claim
    storage.set_timestamp(U256::from(base_time + unbonding_period + 1));
    StorageHandle::enter(&mut storage, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), unbonding_period);

        s.claim_unbonded(validator).unwrap();

        let ctx = storage.clone();
        // Validator received 2000 back
        assert_eq!(
            ctx.balance(validator).unwrap(),
            U256::from(DEFAULT_BALANCE + 2_000)
        );
    });
}

/// `STAKING_ADDRESS` is allow-listed at the precompile boundary because
/// `stake` is payable, so value reaches every selector on the address. The
/// read-only selectors must reject it themselves or a value-carrying view call
/// would strand funds in the balance that backs `claimUnbonded`.
#[test]
fn read_only_selectors_reject_native_value() {
    let validator = address!("0x0000000000000000000000000000000000000001");
    let calls = [
        IStaking::getStakeCall { validator }.abi_encode(),
        IStaking::getTotalStakedCall {}.abi_encode(),
        IStaking::getMinStakeCall {}.abi_encode(),
        IStaking::getUnbondingPeriodCall {}.abi_encode(),
        IStaking::getMaxStakePercentCall {}.abi_encode(),
        IStaking::getSlashedWithdrawalDelayCall {}.abi_encode(),
    ];

    with_staking(|storage, _| {
        for data in &calls {
            let rejected = dispatch(storage.clone(), data, validator, U256::from(1u64));
            assert!(
                matches!(
                    rejected,
                    Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "value-carrying read call must revert, got {rejected:?}"
            );
            dispatch(storage.clone(), data, validator, U256::ZERO)
                .expect("zero-value read call must still succeed");
        }
    });
}

fn staking_session(
    timestamp: u64,
    period: u64,
    body: impl FnOnce(StorageHandle, &mut Staking),
) -> Vec<Log> {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(timestamp));
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = staking_with_unbonding_period(storage.clone(), period);
        body(storage, &mut staking);
    });
    provider.get_ordered_events().to_vec()
}

fn staking_logs(logs: &[Log]) -> Vec<&Log> {
    logs.iter()
        .filter(|log| log.address == STAKING_ADDRESS)
        .collect()
}

fn dispatch_stake(
    storage: StorageHandle<'_>,
    validator: Address,
    amount: u64,
) -> outbe_primitives::error::Result<()> {
    if !ValidatorSet::new(storage.clone()).is_validator(validator)? {
        register_validator(storage.clone(), validator);
    }
    seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
    seed_staking_balance(storage.clone(), amount);
    dispatch(
        storage,
        &IStaking::topupCall {
            validatorAddress: validator,
            amount: U256::from(amount),
        }
        .abi_encode(),
        validator,
        U256::from(amount),
    )?;
    Ok(())
}

fn dispatch_unstake(
    storage: StorageHandle<'_>,
    validator: Address,
    amount: u64,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    dispatch(
        storage,
        &IStaking::unstakeCall {
            amount: U256::from(amount),
        }
        .abi_encode(),
        validator,
        U256::ZERO,
    )
}

fn dispatch_claim(
    storage: StorageHandle<'_>,
    validator: Address,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    dispatch(
        storage,
        &IStaking::claimUnbondedCall {}.abi_encode(),
        validator,
        U256::ZERO,
    )
}

/// Obligation: both stake credits emit the deposited amount and the resulting bonded stake.
#[test]
fn stake_events_report_resulting_bonded_on_both_credit_branches() {
    let validator = address!("0x1111111111111111111111111111111111111111");
    let other = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let mut bonded = U256::ZERO;
    let logs = staking_session(0, 3_600, |storage, staking| {
        staking.config_max_stake_percent.write(0).unwrap();
        dispatch_stake(storage.clone(), validator, 500).unwrap();
        assert!(dispatch_stake(storage.clone(), other, 0).is_err());
        assert!(dispatch(
            storage.clone(),
            &IStaking::topupCall {
                validatorAddress: validator,
                amount: U256::from(500u64),
            }
            .abi_encode(),
            other,
            U256::from(500u64),
        )
        .is_err());
        bonded = staking.get_stake(validator).unwrap();
    });
    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 1);
    assert_eq!(bonded, U256::from(500u64));
    assert_eq!(
        IStaking::Staked::decode_log_data(logs[0]).unwrap(),
        IStaking::Staked {
            validator,
            amount: U256::from(500u64),
            bonded: U256::from(500u64),
        }
    );

    let mut bonded = U256::ZERO;
    let logs = staking_session(0, 3_600, |storage, staking| {
        staking.config_max_stake_percent.write(40).unwrap();
        dispatch_stake(storage.clone(), validator, 500).unwrap();
        assert!(dispatch(
            storage,
            &IStaking::topupCall {
                validatorAddress: validator,
                amount: U256::from(500u64),
            }
            .abi_encode(),
            validator,
            U256::from(500u64),
        )
        .is_err());
        bonded = staking.get_stake(validator).unwrap();
    });
    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 1);
    assert_eq!(bonded, U256::from(500u64));
    assert_eq!(
        IStaking::Staked::decode_log_data(logs[0]).unwrap(),
        IStaking::Staked {
            validator,
            amount: U256::from(500u64),
            bonded: U256::from(500u64),
        }
    );
}

/// Obligation: a zero-rounded queue penalty still reports the new deadline and does not emit `Slashed`.
#[test]
fn deadline_only_queue_change_emits_update_without_slashed() {
    let validator = address!("0x1212121212121212121212121212121212121212");
    let mut amount = U256::ZERO;
    let mut complete = 0u64;
    let logs = staking_session(10_000, 100, |storage, staking| {
        dispatch_stake(storage.clone(), validator, 1).unwrap();
        dispatch_unstake(storage.clone(), validator, 1).unwrap();
        staking.slash_stake(validator, 50).unwrap();
        amount = staking.unbonding_amount.read(&0u32).unwrap();
        complete = staking.unbonding_complete_time.read(&0u32).unwrap();
    });
    assert_eq!(amount, U256::from(1u64));
    assert_eq!(complete, 10_200);
    let logs = staking_logs(&logs);
    assert!(logs
        .iter()
        .all(|log| log.topics().first() != Some(&IStaking::Slashed::SIGNATURE_HASH)));
    let updated = IStaking::UnbondingUpdated::decode_log_data(
        logs.iter()
            .find(|log| log.topics().first() == Some(&IStaking::UnbondingUpdated::SIGNATURE_HASH))
            .expect("deadline change"),
    )
    .unwrap();
    assert_eq!(updated.validator, validator);
    assert_eq!(updated.index, 0);
    assert_eq!(updated.remaining, U256::from(1u64));
    assert_eq!(updated.completeTime, 10_200);
}

/// Obligation: a slash that changes neither money nor a deadline emits nothing.
#[test]
fn unchanged_slash_emits_no_staking_event() {
    let validator = address!("0x1313131313131313131313131313131313131313");
    let logs = staking_session(5_000, 100, |storage, staking| {
        staking.config_min_stake.write(U256::from(100u64)).unwrap();
        dispatch_stake(storage.clone(), validator, 1_000).unwrap();
        dispatch_unstake(storage.clone(), validator, 100).unwrap();
        staking.config_slashed_withdrawal_delay.write(1).unwrap();
        let slashed = staking.slash_stake(validator, 0).unwrap();
        assert_eq!(slashed, U256::ZERO);
        assert_eq!(staking.get_stake(validator).unwrap(), U256::from(900u64));
        assert_eq!(
            staking.unbonding_amount.read(&0u32).unwrap(),
            U256::from(100u64)
        );
        assert_eq!(staking.unbonding_complete_time.read(&0u32).unwrap(), 5_100);
    });
    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|log| log.topics().first()
        != Some(&IStaking::Slashed::SIGNATURE_HASH)
        && log.topics().first() != Some(&IStaking::UnbondingUpdated::SIGNATURE_HASH)));
}

/// Obligation: a reverted public frame drops the stake and log created inside it.
#[test]
fn reverted_stake_frame_keeps_prior_state_and_logs() {
    let validator = address!("0x1414141414141414141414141414141414141414");
    let mut bonded = U256::ZERO;
    let logs = staking_session(0, 3_600, |storage, staking| {
        dispatch_stake(storage.clone(), validator, 100).unwrap();
        let reverted = storage.with_checkpoint(|| {
            dispatch_stake(storage.clone(), validator, 50)?;
            dispatch_unstake(storage.clone(), validator, 10_000)?;
            Ok(())
        });
        assert!(reverted.is_err());
        bonded = staking.get_stake(validator).unwrap();
    });
    assert_eq!(bonded, U256::from(100u64));
    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 1);
    assert_eq!(
        IStaking::Staked::decode_log_data(logs[0]).unwrap(),
        IStaking::Staked {
            validator,
            amount: U256::from(100u64),
            bonded: U256::from(100u64),
        }
    );
}

/// Obligation: retirement of an already-Unbonding validator queues its remaining stake as automatic.
#[test]
fn automatic_retirement_starts_unbonding_for_the_remaining_stake() {
    let survivor = address!("0x00000000000000000000000000000000000000b1");
    let subject = address!("0x00000000000000000000000000000000000000b2");
    let mut remaining = U256::from(1u64);
    let mut queued = U256::ZERO;
    let logs = staking_session(1_000, 3_600, |storage, staking| {
        dispatch_stake(storage.clone(), survivor, MIN_STAKE).unwrap();
        dispatch_stake(storage.clone(), subject, MIN_STAKE).unwrap();
        let mut validators = ValidatorSet::new(storage.clone());
        validators
            .promote_pending_validator_for_test(survivor)
            .unwrap();
        validators
            .promote_pending_validator_for_test(subject)
            .unwrap();
        validators.force_exit_validator(subject).unwrap();
        validators
            .activate_reshared_set(&[survivor], B256::with_last_byte(0x11))
            .unwrap();
        assert!(matches!(
            validators.validator_lifecycle(subject).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));
        staking.process_unbonding(1_000).unwrap();
        remaining = staking.get_stake(subject).unwrap();
        queued = staking.unbonding_amount.read(&0u32).unwrap();
    });
    assert_eq!(remaining, U256::ZERO);
    assert_eq!(queued, U256::from(MIN_STAKE));
    let started: Vec<_> = staking_logs(&logs)
        .into_iter()
        .filter(|log| log.topics().first() == Some(&IStaking::UnbondingStarted::SIGNATURE_HASH))
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        IStaking::UnbondingStarted::decode_log_data(started[0]).unwrap(),
        IStaking::UnbondingStarted {
            validator: subject,
            index: 0,
            amount: U256::from(MIN_STAKE),
            completeTime: 1_000 + 7_200,
            automatic: true,
        }
    );
}

/// Obligation: ordered staking logs reconstruct balances after a claim terminal reuses an index.
#[test]
fn ordered_replay_matches_balances_after_index_reuse() {
    let validator = address!("0x1515151515151515151515151515151515151515");
    let mut bonded = U256::ZERO;
    let mut queued = U256::ZERO;
    let mut count = 0u32;
    let logs = staking_session(10_000, 100, |storage, staking| {
        dispatch_stake(storage.clone(), validator, 10_000).unwrap();
        dispatch_unstake(storage.clone(), validator, 4_000).unwrap();
        dispatch_unstake(storage.clone(), validator, 2_000).unwrap();
        dispatch_claim(storage.clone(), validator).unwrap();
        let slashed = staking.slash_stake(validator, 50).unwrap();
        assert_eq!(slashed, U256::from(5_000u64));
        storage.set_block_timestamp(U256::from(10_100u64)).unwrap();
        dispatch_claim(storage.clone(), validator).unwrap();
        storage.set_block_timestamp(U256::from(10_201u64)).unwrap();
        dispatch_claim(storage.clone(), validator).unwrap();
        dispatch_claim(storage.clone(), validator).unwrap();
        staking.process_unbonding(10_201).unwrap();
        dispatch_unstake(storage.clone(), validator, 100).unwrap();
        bonded = staking.get_stake(validator).unwrap();
        queued = staking.unbonding_amount.read(&0u32).unwrap();
        count = staking.unbonding_count.read().unwrap();
    });

    assert_eq!(bonded, U256::from(1_900u64));
    assert_eq!(queued, U256::from(100u64));
    assert_eq!(count, 1);

    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 9);
    assert_eq!(
        IStaking::Staked::decode_log_data(logs[0]).unwrap().bonded,
        U256::from(10_000u64)
    );
    let first = IStaking::UnbondingStarted::decode_log_data(logs[1]).unwrap();
    assert_eq!(first.index, 0);
    assert_eq!(first.amount, U256::from(4_000u64));
    assert!(!first.automatic);
    assert_eq!(first.completeTime, 10_100);
    let second = IStaking::UnbondingStarted::decode_log_data(logs[2]).unwrap();
    assert_eq!(second.index, 1);
    assert_eq!(second.amount, U256::from(2_000u64));
    assert!(!second.automatic);
    let newest = IStaking::UnbondingUpdated::decode_log_data(logs[3]).unwrap();
    assert_eq!(newest.index, 1);
    assert_eq!(newest.remaining, U256::from(1_000u64));
    assert_eq!(newest.completeTime, 10_200);
    let older = IStaking::UnbondingUpdated::decode_log_data(logs[4]).unwrap();
    assert_eq!(older.index, 0);
    assert_eq!(older.remaining, U256::from(2_000u64));
    assert_eq!(older.completeTime, 10_200);
    assert_eq!(
        IStaking::Slashed::decode_log_data(logs[5]).unwrap(),
        IStaking::Slashed {
            validator,
            bondedSlashed: U256::from(2_000u64),
            unbondingSlashed: U256::from(3_000u64),
        }
    );
    let paid_newest = IStaking::UnbondedClaimed::decode_log_data(logs[6]).unwrap();
    assert_eq!(paid_newest.index, 1);
    assert_eq!(paid_newest.paid, U256::from(1_000u64));
    let paid_older = IStaking::UnbondedClaimed::decode_log_data(logs[7]).unwrap();
    assert_eq!(paid_older.index, 0);
    assert_eq!(paid_older.paid, U256::from(2_000u64));
    let reused = IStaking::UnbondingStarted::decode_log_data(logs[8]).unwrap();
    assert_eq!(reused.index, 0);
    assert_eq!(reused.amount, U256::from(100u64));
    assert_eq!(reused.completeTime, 10_301);
    assert!(!reused.automatic);

    let (replay_bonded, open_amount) = replay_open_queue(&logs);
    assert_eq!(replay_bonded, U256::from(1_900u64));
    assert_eq!(open_amount, U256::from(100u64));
}

/// Obligation: a slash that writes a zero remainder is an update, and the later claim is silent.
/// The next queue life can reuse that index.
#[test]
fn zero_remainder_slash_reuses_index_without_a_claim_event() {
    let validator = address!("0x1616161616161616161616161616161616161616");
    let logs = staking_session(1_000, 100, |storage, staking| {
        staking.config_min_stake.write(U256::from(10u64)).unwrap();
        dispatch_stake(storage.clone(), validator, 100).unwrap();
        dispatch_unstake(storage.clone(), validator, 80).unwrap();
        let slashed = staking.slash_stake(validator, 100).unwrap();
        assert_eq!(slashed, U256::from(100u64));
        storage.set_block_timestamp(U256::from(1_201u64)).unwrap();
        dispatch_claim(storage.clone(), validator).unwrap();
        staking.process_unbonding(1_201).unwrap();
        dispatch_stake(storage.clone(), validator, 10).unwrap();
        dispatch_unstake(storage, validator, 10).unwrap();
    });
    let logs = staking_logs(&logs);
    assert_eq!(logs.len(), 6);
    assert_eq!(
        IStaking::UnbondingUpdated::decode_log_data(logs[2]).unwrap(),
        IStaking::UnbondingUpdated {
            validator,
            index: 0,
            remaining: U256::ZERO,
            completeTime: 1_200,
        }
    );
    assert_eq!(
        IStaking::Slashed::decode_log_data(logs[3]).unwrap(),
        IStaking::Slashed {
            validator,
            bondedSlashed: U256::from(20u64),
            unbondingSlashed: U256::from(80u64),
        }
    );
    assert_eq!(
        IStaking::UnbondingStarted::decode_log_data(logs[5])
            .unwrap()
            .index,
        0
    );
    let (bonded, open_amount) = replay_open_queue(&logs);
    assert_eq!(bonded, U256::ZERO);
    assert_eq!(open_amount, U256::from(10u64));
}

fn replay_open_queue(logs: &[&Log]) -> (U256, U256) {
    let mut bonded = U256::ZERO;
    let mut queue: std::collections::BTreeMap<u32, (U256, bool)> =
        std::collections::BTreeMap::new();
    for log in logs {
        let topic = log.topics().first().copied();
        if topic == Some(IStaking::Staked::SIGNATURE_HASH) {
            let event = IStaking::Staked::decode_log_data(log).unwrap();
            bonded += event.amount;
            assert_eq!(bonded, event.bonded);
        } else if topic == Some(IStaking::UnbondingStarted::SIGNATURE_HASH) {
            let event = IStaking::UnbondingStarted::decode_log_data(log).unwrap();
            if let Some((_, open)) = queue.get(&event.index) {
                assert!(!open);
            }
            bonded -= event.amount;
            queue.insert(event.index, (event.amount, true));
        } else if topic == Some(IStaking::UnbondingUpdated::SIGNATURE_HASH) {
            let event = IStaking::UnbondingUpdated::decode_log_data(log).unwrap();
            let (amount, open) = queue.get_mut(&event.index).unwrap();
            assert!(*open);
            *amount = event.remaining;
            if event.remaining.is_zero() {
                *open = false;
            }
        } else if topic == Some(IStaking::UnbondedClaimed::SIGNATURE_HASH) {
            let event = IStaking::UnbondedClaimed::decode_log_data(log).unwrap();
            let (amount, open) = queue.get_mut(&event.index).unwrap();
            assert!(*open);
            assert_eq!(event.paid, *amount);
            *open = false;
        } else if topic == Some(IStaking::Slashed::SIGNATURE_HASH) {
            let event = IStaking::Slashed::decode_log_data(log).unwrap();
            bonded -= event.bondedSlashed;
        }
    }
    let open_amount = queue
        .values()
        .filter(|(_, open)| *open)
        .map(|(amount, _)| *amount)
        .fold(U256::ZERO, |sum, amount| sum + amount);
    (bonded, open_amount)
}

/// Obligation: the four views return configured native units, seconds, the stored percent, and the effective delay.
#[test]
fn term_getters_return_configured_units_and_effective_delay() {
    staking_session(0, 3_600, |storage, staking| {
        staking.config_max_stake_percent.write(7).unwrap();
        staking.config_slashed_withdrawal_delay.write(90).unwrap();
        let min = dispatch(
            storage.clone(),
            &IStaking::getMinStakeCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getMinStakeCall::abi_decode_returns(&min).unwrap(),
            U256::from(MIN_STAKE)
        );
        staking.config_min_stake.write(U256::ZERO).unwrap();
        let default_min = dispatch(
            storage.clone(),
            &IStaking::getMinStakeCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getMinStakeCall::abi_decode_returns(&default_min).unwrap(),
            crate::contract::DEFAULT_MIN_STAKE
        );
        staking
            .config_min_stake
            .write(U256::from(MIN_STAKE))
            .unwrap();
        let period = dispatch(
            storage.clone(),
            &IStaking::getUnbondingPeriodCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getUnbondingPeriodCall::abi_decode_returns(&period).unwrap(),
            3_600u64
        );
        let percent = dispatch(
            storage.clone(),
            &IStaking::getMaxStakePercentCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getMaxStakePercentCall::abi_decode_returns(&percent).unwrap(),
            7u64
        );
        let configured = dispatch(
            storage.clone(),
            &IStaking::getSlashedWithdrawalDelayCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getSlashedWithdrawalDelayCall::abi_decode_returns(&configured).unwrap(),
            90u64
        );
        staking.config_slashed_withdrawal_delay.write(0).unwrap();
        let effective = dispatch(
            storage,
            &IStaking::getSlashedWithdrawalDelayCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IStaking::getSlashedWithdrawalDelayCall::abi_decode_returns(&effective).unwrap(),
            7_200u64
        );
    });
}

/// Obligation: stored delay zero keeps the typed overflow of `2 * unbondingPeriod`.
#[test]
fn slashed_withdrawal_delay_getter_keeps_typed_overflow() {
    staking_session(0, 3_600, |storage, staking| {
        staking
            .config_unbonding_period
            .write(u64::MAX / 2 + 1)
            .unwrap();
        staking.config_slashed_withdrawal_delay.write(0).unwrap();
        let error = dispatch(
            storage,
            &IStaking::getSlashedWithdrawalDelayCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        match error {
            outbe_primitives::error::PrecompileError::RevertBytes(bytes) => {
                IStaking::SlashedWithdrawalDelayOverflow::abi_decode(&bytes).unwrap();
            }
            other => panic!("typed overflow, got {other:?}"),
        }
    });
}

#[test]
fn stake_registers_with_optional_or_v1_p2p_and_rejects_below_min_stake() {
    use outbe_primitives::addresses::VALIDATOR_SET_ADDRESS;
    use outbe_primitives::consensus_p2p::{encode_v1, P2pAddress, P2P_ADDRESS_VERSION_V1};
    use outbe_validatorset::precompile::IValidatorSet;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let v1 = address!("0x2121212121212121212121212121212121212121");
    let v2 = address!("0x2222222222222222222222222222222222222222");
    let (pk1, node1, sig1) = test_bls_registration_material(v1, 0x21);
    let (pk2, node2, sig2) = test_bls_registration_material(v2, 0x22);
    let p2p_bytes = encode_v1(&P2pAddress::Symmetric(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        30400,
    )));

    let logs = staking_session(0, 3_600, |storage, staking| {
        ValidatorSet::new(storage.clone())
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();

        // 1. Below min_stake reverts with BondedStakeBelowMinimum and writes nothing.
        let sub_min = U256::from(MIN_STAKE - 1);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        let err = staking
            .stake(v1, v1, sub_min, (&pk1, node1, &sig1), (0, &[]))
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref()
                    == IValidatorSet::BondedStakeBelowMinimum {
                        bonded: sub_min,
                        minimum: U256::from(MIN_STAKE),
                    }
                    .abi_encode()
        ));
        assert_eq!(staking.get_stake(v1).unwrap(), U256::ZERO);
        assert!(!ValidatorSet::new(storage.clone()).is_validator(v1).unwrap());

        // 2. Non-empty p2p_encoded with version != 1 reverts with UnsupportedP2pAddressVersion.
        let err = staking
            .stake(
                v1,
                v1,
                U256::from(MIN_STAKE),
                (&pk1, node1, &sig1),
                (2, &p2p_bytes),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref()
                    == IValidatorSet::UnsupportedP2pAddressVersion { version: 2 }.abi_encode()
        ));

        // 3. Empty p2p_encoded registers with P2pInfo::Unset (even if p2p_version is 0).
        staking
            .stake(
                v1,
                v1,
                U256::from(MIN_STAKE),
                (&pk1, node1, &sig1),
                (0, &[]),
            )
            .unwrap();
        assert_eq!(
            ValidatorSet::new(storage.clone())
                .get_p2p_address(v1)
                .unwrap(),
            None
        );

        // 4. Non-empty v1 p2p_encoded registers and stores P2P address.
        seed_staking_balance(storage.clone(), MIN_STAKE);
        staking
            .stake(
                v2,
                v2,
                U256::from(MIN_STAKE),
                (&pk2, node2, &sig2),
                (P2P_ADDRESS_VERSION_V1, &p2p_bytes),
            )
            .unwrap();
        assert_eq!(
            ValidatorSet::new(storage).get_p2p_address(v2).unwrap(),
            Some((P2P_ADDRESS_VERSION_V1, p2p_bytes.clone()))
        );
    });

    // Both v1 and v2 emitted ValidatorRegistered + ValidatorPending on ValidatorSet, and Staked on Staking.
    let vs_logs: Vec<_> = logs
        .iter()
        .filter(|log| log.address == VALIDATOR_SET_ADDRESS)
        .collect();
    assert_eq!(vs_logs.len(), 4);
    assert_eq!(
        vs_logs[0].topics().first(),
        Some(&IValidatorSet::ValidatorRegistered::SIGNATURE_HASH)
    );
    assert_eq!(
        vs_logs[1].topics().first(),
        Some(&IValidatorSet::ValidatorPending::SIGNATURE_HASH)
    );
    let st_logs = staking_logs(&logs);
    assert_eq!(st_logs.len(), 2);
}

#[test]
fn topup_requires_registered_eligible_lifecycle_and_emits_only_staked() {
    use outbe_primitives::addresses::VALIDATOR_SET_ADDRESS;
    use outbe_validatorset::precompile::IValidatorSet;

    let validator = address!("0x2323232323232323232323232323232323232323");
    let (pk, node, sig) = test_bls_registration_material(validator, 0x23);

    let logs = staking_session(0, 3_600, |storage, staking| {
        ValidatorSet::new(storage.clone())
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();

        // Absent validator rejected on topup with ValidatorNotRegistered.
        seed_staking_balance(storage.clone(), MIN_STAKE * 3);
        let err = staking
            .topup(validator, validator, U256::from(100u64))
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IValidatorSet::ValidatorNotRegistered {}.abi_encode()
        ));

        // Initial stake registers into WaitingForReadiness.
        staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk, node, &sig),
                (0, &[]),
            )
            .unwrap();

        // Duplicate stake on WaitingForReadiness rejected with ValidatorAlreadyRegistered.
        let err = staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk, node, &sig),
                (0, &[]),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IValidatorSet::ValidatorAlreadyRegistered {}.abi_encode()
        ));

        // Topup in WaitingForReadiness succeeds.
        staking
            .topup(validator, validator, U256::from(250u64))
            .unwrap();
        assert_eq!(
            staking.get_stake(validator).unwrap(),
            U256::from(MIN_STAKE + 250)
        );
    });

    // Initial stake emits one ValidatorRegistered and one ValidatorPending. Topup emits only Staked.
    let vs_logs: Vec<_> = logs
        .iter()
        .filter(|log| log.address == VALIDATOR_SET_ADDRESS)
        .collect();
    assert_eq!(vs_logs.len(), 2);
    let st_logs = staking_logs(&logs);
    assert_eq!(st_logs.len(), 2);
}

#[test]
fn unstake_enforces_min_stake_floor_and_full_exit_in_jail_and_zero_bond_exit() {
    use outbe_validatorset::precompile::IValidatorSet;

    let v_jailed = address!("0x2424242424242424242424242424242424242424");
    let v_slashed = address!("0x2525252525252525252525252525252525252525");

    let logs = staking_session(0, 3_600, |storage, staking| {
        seed_staking_balance(storage.clone(), MIN_STAKE * 4);
        stake_registered(
            storage.clone(),
            staking,
            v_jailed,
            U256::from(MIN_STAKE * 2),
        )
        .unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .activate_validator_via_boundary_for_test(v_jailed)
            .unwrap();
        val_set.jail_validator(v_jailed).unwrap();

        // In JailRetained, partial unstake (even leaving >= MIN_STAKE) must revert with
        // JailedValidatorMustFullyUnstakeForExit.
        let err = staking
            .unstake(v_jailed, U256::from(MIN_STAKE))
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref()
                    == IValidatorSet::JailedValidatorMustFullyUnstakeForExit {}
                        .abi_encode()
        ));

        // Boundary exclusion moves the validator to Jail. Partial unstake still reverts with JailedValidatorMustFullyUnstakeForExit.
        val_set
            .test_activate_validated_boundary_set(&[], B256::ZERO, 1)
            .unwrap();
        let err = staking
            .unstake(v_jailed, U256::from(MIN_STAKE))
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref()
                    == IValidatorSet::JailedValidatorMustFullyUnstakeForExit {}
                        .abi_encode()
        ));

        // While current > 0, unstake(0) reverts with AmountZero.
        let err = staking.unstake(v_jailed, U256::ZERO).unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IStaking::AmountZero {}.abi_encode()
        ));

        // 100% slash of v_slashed in WaitingForReadiness leaves current == 0.
        stake_registered(storage.clone(), staking, v_slashed, U256::from(MIN_STAKE)).unwrap();
        staking.slash_stake(v_slashed, 100).unwrap();
        assert_eq!(staking.get_stake(v_slashed).unwrap(), U256::ZERO);

        // unstake(0) when current == 0 and no pending unbonding transitions WaitingForReadiness -> Unbonding -> Inactive
        // without creating an unbonding entry or emitting UnbondingStarted.
        let count_before = staking.unbonding_count.read().unwrap();
        staking.unstake(v_slashed, U256::ZERO).unwrap();
        assert_eq!(staking.unbonding_count.read().unwrap(), count_before);
        assert!(matches!(
            ValidatorSet::new(storage)
                .validator_lifecycle(v_slashed)
                .unwrap(),
            ValidatorLifecycle::Inactive(_)
        ));
    });

    assert!(staking_logs(&logs)
        .iter()
        .all(|log| log.topics().first() != Some(&IStaking::UnbondingStarted::SIGNATURE_HASH)));
}

#[test]
fn reregistration_via_stake_blocks_same_epoch_even_when_cooldown_zero() {
    use outbe_validatorset::precompile::IValidatorSet;
    use outbe_validatorset::test_support::StorageOverrides;
    use outbe_validatorset::EpochSnapshot;

    let validator = address!("0x2626262626262626262626262626262626262626");
    let (pk1, node, sig1) = test_bls_registration_material(validator, 0x26);
    let (pk2, _, sig2) = test_bls_registration_material(validator, 0x27);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(15);
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = staking_with_unbonding_period(storage.clone(), 0);
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();
        val_set
            .test_set_epoch_snapshot(EpochSnapshot {
                number: U256::from(1u64),
                start_timestamp: 100,
                start_block: 10,
                length_blocks: 10,
            })
            .unwrap();

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE * 2);
        staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk1, node, &sig1),
                (0, &[]),
            )
            .unwrap();

        // Exit in block 15 (> epoch_start_block 10) and claim immediately (period = 0) -> Inactive.
        staking.unstake(validator, U256::from(MIN_STAKE)).unwrap();
        staking.claim_unbonded(validator).unwrap();
        assert!(matches!(
            ValidatorSet::new(storage.clone())
                .validator_lifecycle(validator)
                .unwrap(),
            ValidatorLifecycle::Inactive(_)
        ));

        // Same-epoch re-registration (deactivated_at 15 > epoch_start_block 10) must revert
        // even with cooldown == 0.
        let err = staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk2, node, &sig2),
                (0, &[]),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IValidatorSet::ReregistrationCooldownNotExpired {}.abi_encode()
        ));
    });

    // Advance to next epoch (epoch_start_block = 20 >= deactivated_at 15) -> re-registration succeeds.
    provider.set_block_number(20);
    StorageHandle::enter(&mut provider, |storage| {
        ValidatorSet::new(storage.clone())
            .update_epoch(200, 20)
            .unwrap();
        let mut staking = Staking::new(storage.clone());
        staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk2, node, &sig2),
                (0, &[]),
            )
            .unwrap();
        assert!(matches!(
            ValidatorSet::new(storage)
                .validator_lifecycle(validator)
                .unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
    });
}

#[test]
fn reregistration_via_stake_blocks_epoch_zero_with_zero_and_elapsed_nonzero_cooldown() {
    let validator = address!("0x2828282828282828282828282828282828282828");
    let (pk1, node, sig1) = test_bls_registration_material(validator, 0x28);
    let (pk2, _, sig2) = test_bls_registration_material(validator, 0x29);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(5);
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = staking_with_unbonding_period(storage.clone(), 0);
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
            .unwrap();
        // `epoch_start_block` intentionally stays at 0 (genesis epoch 0).
        assert_eq!(val_set.epoch_snapshot().unwrap().start_block, 0);

        seed_balance(storage.clone(), validator, DEFAULT_BALANCE);
        seed_staking_balance(storage.clone(), MIN_STAKE * 2);
        staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk1, node, &sig1),
                (0, &[]),
            )
            .unwrap();

        // Exit at block 5 (`deactivated_at = 5 > epoch_start_block = 0`).
        staking.unstake(validator, U256::from(MIN_STAKE)).unwrap();
    });

    // Case 1 (`epoch_start_block == 0`, `cooldown == 0` at block 6): claim -> Inactive,
    // and `Staking::stake` reverts with `ReregistrationCooldownNotExpired`.
    provider.set_block_number(6);
    StorageHandle::enter(&mut provider, |storage| {
        let mut staking = Staking::new(storage.clone());
        staking.claim_unbonded(validator).unwrap();
        assert!(matches!(
            ValidatorSet::new(storage.clone())
                .validator_lifecycle(validator)
                .unwrap(),
            ValidatorLifecycle::Inactive(_)
        ));

        let err = staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk2, node, &sig2),
                (0, &[]),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IStaking::ReregistrationCooldownNotExpired {}.abi_encode()
        ));
    });

    // Case 2 (`epoch_start_block == 0`, `cooldown = 2` already elapsed by blocks at block 10 >= 5 + 2):
    // still rejected in epoch 0 because `deactivated_at (5) > epoch_start_block (0)`.
    provider.set_block_number(10);
    StorageHandle::enter(&mut provider, |storage| {
        let val_set = ValidatorSet::new(storage.clone());
        val_set.config_reregistration_cooldown.write(2).unwrap();
        let mut staking = Staking::new(storage.clone());
        let err = staking
            .stake(
                validator,
                validator,
                U256::from(MIN_STAKE),
                (&pk2, node, &sig2),
                (0, &[]),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::RevertBytes(ref bytes)
                if bytes.as_ref() == IStaking::ReregistrationCooldownNotExpired {}.abi_encode()
        ));
    });
}

#[test]
fn precompile_dispatch_rejects_msg_value_mismatch_for_stake_and_topup() {
    let validator = address!("0x2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a");
    let (pk, node, sig) = test_bls_registration_material(validator, 0x2a);
    let amount = U256::from(MIN_STAKE);

    // 1. `stakeCall` with `value = amount - 1` and `value = amount + 1`
    for value in [amount - U256::ONE, amount + U256::ONE] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        StorageHandle::enter(&mut provider, |storage| {
            let _staking = staking_with_unbonding_period(storage.clone(), 3600);
            ValidatorSet::new(storage.clone())
                .test_configure_registry(address!("0xffffffffffffffffffffffffffffffffffffffff"))
                .unwrap();
            seed_staking_balance_u256(storage.clone(), value);
        });
        let storage_before = provider.storage.clone();
        let events_before = provider.get_ordered_events().to_vec();
        StorageHandle::enter(&mut provider, |storage| {
            let call = IStaking::stakeCall {
                validatorAddress: validator,
                amount,
                consensusPubkey: pk.to_vec().into(),
                radicleNodeId: node,
                blsRegistrationSignature: sig.to_vec().into(),
                p2pVersion: 0,
                p2pEncoded: Vec::<u8>::new().into(),
            };
            let err = dispatch(storage.clone(), &call.abi_encode(), validator, value).unwrap_err();
            let expected = IStaking::MsgValueMismatch {}.abi_encode();
            assert!(
                matches!(err, PrecompileError::RevertBytes(ref bytes) if bytes.as_ref() == expected),
                "unexpected error for stake value={value}: {err:?}"
            );
            let s = Staking::new(storage.clone());
            assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
            assert_eq!(s.get_total_staked().unwrap(), U256::ZERO);
            assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), value);
            assert_eq!(storage.balance(validator).unwrap(), U256::ZERO);
            assert!(!ValidatorSet::new(storage).is_validator(validator).unwrap());
        });
        assert_eq!(provider.storage, storage_before);
        assert_eq!(provider.get_balance(STAKING_ADDRESS), value);
        assert_eq!(provider.get_balance(validator), U256::ZERO);
        assert_eq!(provider.get_ordered_events(), events_before);
    }

    // 2. `topupCall` with `value = amount - 1` and `value = amount + 1`
    for value in [amount - U256::ONE, amount + U256::ONE] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut staking = staking_with_unbonding_period(storage.clone(), 3600);
            seed_staking_balance_u256(storage.clone(), amount + value);
            stake_registered(storage.clone(), &mut staking, validator, amount).unwrap();
        });
        let storage_before = provider.storage.clone();
        let events_before = provider.get_ordered_events().to_vec();
        StorageHandle::enter(&mut provider, |storage| {
            let call = IStaking::topupCall {
                validatorAddress: validator,
                amount,
            };
            let err = dispatch(storage.clone(), &call.abi_encode(), validator, value).unwrap_err();
            let expected = IStaking::MsgValueMismatch {}.abi_encode();
            assert!(
                matches!(err, PrecompileError::RevertBytes(ref bytes) if bytes.as_ref() == expected),
                "unexpected error for topup value={value}: {err:?}"
            );
            let s = Staking::new(storage.clone());
            assert_eq!(s.get_stake(validator).unwrap(), amount);
            assert_eq!(s.get_total_staked().unwrap(), amount);
            assert_eq!(storage.balance(STAKING_ADDRESS).unwrap(), amount + value);
            assert_eq!(storage.balance(validator).unwrap(), U256::ZERO);
        });
        assert_eq!(provider.storage, storage_before);
        assert_eq!(provider.get_balance(STAKING_ADDRESS), amount + value);
        assert_eq!(provider.get_balance(validator), U256::ZERO);
        assert_eq!(provider.get_ordered_events(), events_before);
    }
}

#[test]
fn claim_unbonded_rolls_back_all_writes_when_transfer_balance_fails() {
    let validator = address!("0x2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), 100);
        seed_staking_balance(storage.clone(), MIN_STAKE);
        stake_registered(storage.clone(), &mut s, validator, U256::from(MIN_STAKE)).unwrap();
        s.unstake(validator, U256::from(MIN_STAKE)).unwrap();
        // Mature the entry and drain STAKING_ADDRESS so `transfer_balance` fails after queue unlinking.
        storage.set_block_timestamp(U256::from(200u64)).unwrap();
        storage.set_balance(STAKING_ADDRESS, U256::ZERO).unwrap();
    });
    let events_before = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        let val_set = ValidatorSet::new(storage.clone());
        let state_before = val_set.validator_state(validator).unwrap();

        assert!(s.claim_unbonded(validator).is_err());

        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 1);
        assert_eq!(s.per_val_unbonding_len.read(&validator).unwrap(), 1);
        assert_eq!(s.unbonding_validator.read(&0).unwrap(), validator);
        assert_eq!(s.unbonding_amount.read(&0).unwrap(), U256::from(MIN_STAKE));
        assert_eq!(s.unbonding_complete_time.read(&0).unwrap(), 100);
        assert_eq!(s.unbonding_next.read(&0).unwrap(), 0);
        assert_eq!(val_set.validator_state(validator).unwrap(), state_before);
    });
    assert_eq!(provider.get_ordered_events(), events_before);
}

#[test]
fn process_unbonding_rolls_back_stake_debit_when_enqueue_fails() {
    let validator = address!("0x2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c");
    let survivor = address!("0x2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = staking_with_unbonding_period(storage.clone(), 100);
        seed_staking_balance(storage.clone(), MIN_STAKE * 2);
        stake_registered(storage.clone(), &mut s, survivor, U256::from(MIN_STAKE)).unwrap();
        stake_registered(storage.clone(), &mut s, validator, U256::from(MIN_STAKE)).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .promote_pending_validator_for_test(survivor)
            .unwrap();
        val_set
            .promote_pending_validator_for_test(validator)
            .unwrap();
        val_set.deactivate_validator(validator, validator).unwrap();
        val_set
            .activate_reshared_set(&[survivor], B256::with_last_byte(0x55))
            .unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));
        // Force `enqueue_unbonding` to fail with `UnbondingIndexOverflow` after `stake_amount` and
        // `total_staked` are debited inside `retire_unbonding_validator`.
        s.unbonding_count.write(u32::MAX).unwrap();
    });
    let events_before = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        let val_set = ValidatorSet::new(storage.clone());
        let state_before = val_set.validator_state(validator).unwrap();
        let total_before = s.get_total_staked().unwrap();

        let err = s.process_unbonding(500).unwrap_err();
        let expected = IStaking::UnbondingIndexOverflow {}.abi_encode();
        assert!(
            matches!(err, PrecompileError::RevertBytes(ref bytes) if bytes.as_ref() == expected),
            "unexpected error: {err:?}"
        );

        assert_eq!(s.get_stake(validator).unwrap(), U256::from(MIN_STAKE));
        assert_eq!(s.get_total_staked().unwrap(), total_before);
        assert_eq!(s.unbonding_count.read().unwrap(), u32::MAX);
        assert_eq!(s.per_val_unbonding_head.read(&validator).unwrap(), 0);
        assert_eq!(s.per_val_unbonding_len.read(&validator).unwrap(), 0);
        assert_eq!(val_set.validator_state(validator).unwrap(), state_before);
    });
    assert_eq!(provider.get_ordered_events(), events_before);
}

#[test]
fn istaking_abi_includes_all_transitive_validator_set_errors() {
    use alloy_sol_types::SolInterface;
    use outbe_validatorset::errors::ValidatorSetError;

    let addr = address!("0x1111111111111111111111111111111111111111");
    let other = address!("0x2222222222222222222222222222222222222222");
    let reachable_errors: [ValidatorSetError; 63] = [
        ValidatorSetError::UnauthorizedOwnerOrValidator,
        ValidatorSetError::ValidatorAlreadyOperationalDelegate,
        ValidatorSetError::ConsensusPubkeyWrongLength,
        ValidatorSetError::ConsensusPubkeyConversionFailed,
        ValidatorSetError::BlsPopWrongLength,
        ValidatorSetError::BlsSignatureConversionFailed,
        ValidatorSetError::InvalidBlsPublicKey,
        ValidatorSetError::InvalidBlsSignature,
        ValidatorSetError::InvalidBlsRegistrationSignature,
        ValidatorSetError::RegistrationRequiresBlsPop,
        ValidatorSetError::UnsupportedP2pAddressVersion { version: 9 },
        ValidatorSetError::P2pAddressPayloadTooLong { max_len: 128 },
        ValidatorSetError::InvalidP2pAddress {
            reason: "bad".to_string(),
        },
        ValidatorSetError::ZeroConsensusPublicKey,
        ValidatorSetError::ZeroRadicleNodeId,
        ValidatorSetError::RadicleNodeIdAlreadyRegistered,
        ValidatorSetError::ConsensusPubkeyAlreadyRegistered,
        ValidatorSetError::InactiveValidatorMustKeepRadicleNodeId,
        ValidatorSetError::InconsistentRadicleNodeIdReverseBinding,
        ValidatorSetError::UnregisteredValidatorRetainsRadicleReservation,
        ValidatorSetError::ValidatorNotRegistered,
        ValidatorSetError::ValidatorAlreadyRegistered,
        ValidatorSetError::ValidatorIsAlreadyRegistered,
        ValidatorSetError::ValidatorIsInactive,
        ValidatorSetError::InactiveValidatorMustReregisterBeforeStaking,
        ValidatorSetError::CannotIncreaseStakeWhileExitingOrUnbonding,
        ValidatorSetError::ReregistrationCooldownHeightOverflow,
        ValidatorSetError::ReregistrationCooldownNotExpired,
        ValidatorSetError::MaxValidatorsReached,
        ValidatorSetError::ValidatorCountOverflow,
        ValidatorSetError::ValidatorRegistryIndexZero,
        ValidatorSetError::BondedStakeBelowMinimum {
            bonded: U256::from(1u64),
            minimum: U256::from(2u64),
        },
        ValidatorSetError::BondedStakeNotBelowMinimum {
            bonded: U256::from(2u64),
            minimum: U256::from(1u64),
        },
        ValidatorSetError::JailedValidatorMustFullyUnstakeForExit,
        ValidatorSetError::JailedValidatorStillRetained,
        ValidatorSetError::UnjailRequiresJailedStatus { status: 1 },
        ValidatorSetError::UnjailCooldownHeightOverflow,
        ValidatorSetError::UnjailCooldownNotElapsed {
            ready_at: 10,
            current_height: 5,
        },
        ValidatorSetError::CannotCompleteUnbondingWithRemainingStakeOrClaim,
        ValidatorSetError::UnknownValidatorStatus { status: 99 },
        ValidatorSetError::CorruptValidatorState {
            address: addr,
            detail: "corrupt".to_string(),
        },
        ValidatorSetError::UnregisteredLifecycleNoStatus,
        ValidatorSetError::ValidatorDeactivationHeightZero,
        ValidatorSetError::RegisteredValidatorMissingHistory,
        ValidatorSetError::RegisteredValidatorMissingConsensusPublicKey,
        ValidatorSetError::RegisteredValidatorMissingConsensusPubkey,
        ValidatorSetError::ValidatorRegistryIndexExceedsCount {
            addr,
            index: 5,
            count: 3,
        },
        ValidatorSetError::ValidatorRegistryForwardIndexMismatch {
            index: 1,
            expected: addr,
            actual: other,
        },
        ValidatorSetError::ValidatorConsensusPubkeyReverseMismatch { addr, owner: other },
        ValidatorSetError::LifecycleTransitionChangedRegistryIdentity,
        ValidatorSetError::CannotPersistAbsentLifecycle,
        ValidatorSetError::RegisteredValidatorMissingRegistryIndex,
        ValidatorSetError::DeactivationHeightZeroSentinel,
        ValidatorSetError::ActiveHistoryHasDeactivationHeight,
        ValidatorSetError::RetainedJailHistoryHeightMismatch,
        ValidatorSetError::JailHistoryHeightMismatch,
        ValidatorSetError::ExitingHistoryMissingDeactivationHeight,
        ValidatorSetError::UnbondingHistoryMissingDeactivationHeight,
        ValidatorSetError::InactiveHistoryMissingDeactivationHeight,
        ValidatorSetError::UnbondingEndHintZeroSentinel,
        ValidatorSetError::ValidatorJailHeightZero,
        ValidatorSetError::InactiveValidatorMissingDeactivationHeight { addr },
        ValidatorSetError::InactiveCleanupHeightOverflow,
    ];

    let exported_abi = include_str!("../../../../contracts/precompiles/abi-export/IStaking.json");
    let abi_json: serde_json::Value =
        serde_json::from_str(exported_abi).expect("IStaking.json must be valid JSON");
    let entries = abi_json
        .as_array()
        .expect("IStaking.json root must be a JSON array");
    let exported_error_selectors: std::collections::HashSet<(String, [u8; 4])> = entries
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("error"))
        .map(|item| {
            let name = item
                .get("name")
                .and_then(|n| n.as_str())
                .expect("error entry must have name");
            let inputs = item
                .get("inputs")
                .and_then(|i| i.as_array())
                .expect("error entry must have inputs array");
            let param_types: Vec<&str> = inputs
                .iter()
                .map(|inp| {
                    inp.get("type")
                        .and_then(|t| t.as_str())
                        .expect("error input must have canonical type")
                })
                .collect();
            let canonical_sig = format!("{name}({})", param_types.join(","));
            let hash = alloy_primitives::keccak256(canonical_sig.as_bytes());
            let mut selector = [0u8; 4];
            selector.copy_from_slice(&hash[..4]);
            (name.to_string(), selector)
        })
        .collect();

    for vs_err in reachable_errors {
        let precompile_err: PrecompileError = vs_err.clone().into();
        let PrecompileError::RevertBytes(bytes) = precompile_err else {
            panic!("expected RevertBytes for {vs_err:?}, got {precompile_err:?}");
        };
        let decoded = IStaking::IStakingErrors::abi_decode(&bytes)
            .unwrap_or_else(|e| panic!("IStakingErrors failed to decode {vs_err:?}: {e}"));
        assert_eq!(decoded.abi_encode(), bytes.as_ref());
        let debug_str = format!("{decoded:?}");
        let error_name = debug_str
            .split(['(', ' ', '{'])
            .next()
            .expect("error variant name");
        let selector: [u8; 4] = bytes[..4].try_into().expect("4-byte error selector");
        assert!(
            exported_error_selectors.contains(&(error_name.to_string(), selector)),
            "exported IStaking.json missing error {error_name} with selector 0x{}",
            alloy_primitives::hex::encode(selector)
        );
    }
}
