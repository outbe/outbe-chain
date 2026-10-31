//! Property tests and bounded-queue invariant tests for `outbe-staking`
//! (`OIP-00140` `T-01`, `T-05`, `T-07`, and `OIP-00266`).

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolError;
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
use outbe_promislimit::PromisLimitContract;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::ValidatorLifecycle;
use proptest::prelude::*;

use crate::contract::{Staking, MAX_UNBONDING_ENTRIES_PER_VALIDATOR};
use crate::errors::StakingError;
use crate::precompile::IStaking;
use crate::tests::{
    register_validator, seed_balance, seed_staking_balance, seed_staking_balance_u256,
    stake_registered, test_bls_registration_material, with_staking, MIN_STAKE,
};

fn physical_unbonding_len(staking: &Staking<'_>, validator: Address) -> u32 {
    let mut current_stored = staking.per_val_unbonding_head.read(&validator).unwrap();
    let mut count = 0u32;
    while current_stored != 0 {
        let idx = current_stored - 1;
        count += 1;
        current_stored = staking.unbonding_next.read(&idx).unwrap();
    }
    count
}

fn active_unbonding_sum(staking: &Staking<'_>) -> U256 {
    let count = staking.unbonding_count.read().unwrap();
    let mut sum = U256::ZERO;
    for idx in 0..count {
        sum += staking.unbonding_amount.read(&idx).unwrap();
    }
    sum
}

#[test]
fn partial_unstake_is_capped_at_63_and_reserves_64th_slot_for_full_or_automatic_exit() {
    let validator = Address::repeat_byte(0x41);
    let max_partial = MAX_UNBONDING_ENTRIES_PER_VALIDATOR - 1;

    // Case A: 63 partial unstakes succeed, 64th partial unstake reverts,
    // and a full voluntary exit succeeds in the 64th slot.
    with_staking(|storage, s| {
        let initial_stake = U256::from(2_000u64);
        seed_staking_balance_u256(storage.clone(), initial_stake);
        stake_registered(storage.clone(), s, validator, initial_stake).unwrap();

        for _ in 0..max_partial {
            s.unstake(validator, U256::ONE).unwrap();
        }
        assert_eq!(
            s.per_val_unbonding_len.read(&validator).unwrap(),
            max_partial
        );
        assert_eq!(physical_unbonding_len(s, validator), max_partial);

        // Staking rejects the 64th partial unstake.
        let expected_err = IStaking::TooManyUnbondingEntries {
            validator,
            maxEntries: max_partial,
        }
        .abi_encode();
        let err = s.unstake(validator, U256::ONE).unwrap_err();
        assert!(
            matches!(err, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected_err)
        );

        // Staking also rejects `topup` while 63 entries remain unclaimed.
        let topup_err = s.topup(validator, validator, U256::ONE).unwrap_err();
        assert!(
            matches!(topup_err, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected_err)
        );

        // Full voluntary unstake uses the reserved 64th slot and succeeds.
        let remaining = s.get_stake(validator).unwrap();
        s.unstake(validator, remaining).unwrap();
        assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
        assert_eq!(
            s.per_val_unbonding_len.read(&validator).unwrap(),
            MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        );
        assert_eq!(
            physical_unbonding_len(s, validator),
            MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        );
    });

    // Case B: 63 partial unstakes on an Active validator, followed by a slash below
    // min_stake and automatic retirement in `process_unbonding`, uses the 64th slot
    // without halting the block hook.
    with_staking(|storage, s| {
        let survivor = Address::repeat_byte(0x40);
        let initial_stake = U256::from(1_063u64);
        seed_staking_balance_u256(storage.clone(), initial_stake * U256::from(2u64));
        stake_registered(storage.clone(), s, survivor, initial_stake).unwrap();
        stake_registered(storage.clone(), s, validator, initial_stake).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        val_set
            .promote_pending_validator_for_test(survivor)
            .unwrap();
        val_set
            .promote_pending_validator_for_test(validator)
            .unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Active(_)
        ));

        for _ in 0..max_partial {
            s.unstake(validator, U256::ONE).unwrap();
        }
        assert_eq!(s.get_stake(validator).unwrap(), U256::from(1_000u64));
        assert_eq!(
            s.per_val_unbonding_len.read(&validator).unwrap(),
            max_partial
        );

        // Slash 10%: remaining stake becomes 900 < min_stake (1000), moving Active -> Exiting.
        s.slash_stake(validator, 10).unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));
        val_set
            .activate_reshared_set(&[survivor], alloy_primitives::B256::with_last_byte(0x11))
            .unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Unbonding(_)
        ));

        // Pre-execution hook retires the remaining 900 units into the 64th slot without error.
        s.process_unbonding(100).unwrap();
        assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
        assert_eq!(
            s.per_val_unbonding_len.read(&validator).unwrap(),
            MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        );
        assert_eq!(
            physical_unbonding_len(s, validator),
            MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        );
    });
}

#[test]
fn claim_unbonded_decrements_counter_for_zero_amount_slashed_entries_and_unblocks_restake() {
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    let validator = Address::repeat_byte(0x42);
    let max_partial = MAX_UNBONDING_ENTRIES_PER_VALIDATOR - 1;
    let (pk1, radicle_node_id, sig1) = test_bls_registration_material(validator, 0x42);
    let (pk2, _, sig2) = test_bls_registration_material(validator, 0x43);
    let initial_stake = U256::from(2_000u64);
    let min_stake = U256::from(MIN_STAKE);

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    let (initial_index, count_before) = StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        s.config_min_stake.write(min_stake).unwrap();
        s.config_unbonding_period.write(3600).unwrap();
        seed_staking_balance_u256(storage.clone(), initial_stake + min_stake);
        seed_balance(storage.clone(), validator, 1_000_000);
        ValidatorSet::new(storage.clone())
            .test_configure_registry(Address::repeat_byte(0xff))
            .unwrap();
        s.stake(
            validator,
            validator,
            initial_stake,
            (&pk1, radicle_node_id, &sig1),
            (0, &[]),
        )
        .unwrap();
        let initial_index = ValidatorSet::new(storage.clone())
            .validator_state(validator)
            .unwrap()
            .registry_index();
        let count_before = ValidatorSet::new(storage.clone())
            .validator_count()
            .unwrap();

        for _ in 0..max_partial {
            s.unstake(validator, U256::ONE).unwrap();
        }
        let remaining = s.get_stake(validator).unwrap();
        s.unstake(validator, remaining).unwrap();
        assert_eq!(
            s.per_val_unbonding_len.read(&validator).unwrap(),
            MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        );

        // 100% slash zeroes all 64 unbonding entries and transitions validator to Inactive.
        s.slash_stake(validator, 100).unwrap();
        let mut val_set = ValidatorSet::new(storage.clone());
        s.finalize_inactive_if_complete(&mut val_set, validator)
            .unwrap();
        assert!(matches!(
            val_set.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::Inactive(_)
        ));
        storage.set_block_timestamp(U256::from(8_000u64)).unwrap();
        (initial_index, count_before)
    });

    // Advance past the slashed withdrawal delay (2 * 3600 = 7200) without claiming.
    // Calling `s.stake(...)` while 64 physical nodes remain linked reverts with
    // `TooManyUnbondingEntries { validator, maxEntries: 63 }` before `register_with_initial_stake`
    // and leaves all storage, balances, and events unchanged.
    let storage_before_reject = provider.storage.clone();
    let staking_bal_before_reject = provider.get_balance(STAKING_ADDRESS);
    let val_bal_before_reject = provider.get_balance(validator);
    let events_before_reject = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        let expected_err = IStaking::TooManyUnbondingEntries {
            validator,
            maxEntries: max_partial,
        }
        .abi_encode();
        let err = s
            .stake(
                validator,
                validator,
                min_stake,
                (&pk2, radicle_node_id, &sig2),
                (0, &[]),
            )
            .unwrap_err();
        assert!(
            matches!(err, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected_err)
        );
        assert_eq!(s.get_stake(validator).unwrap(), U256::ZERO);
        assert_eq!(s.get_total_staked().unwrap(), U256::ZERO);
    });
    assert_eq!(provider.storage, storage_before_reject);
    assert_eq!(
        provider.get_balance(STAKING_ADDRESS),
        staking_bal_before_reject
    );
    assert_eq!(provider.get_balance(validator), val_bal_before_reject);
    assert_eq!(provider.get_ordered_events(), events_before_reject);

    StorageHandle::enter(&mut provider, |storage| {
        let mut s = Staking::new(storage.clone());
        let mut val_set = ValidatorSet::new(storage);
        // Claiming unlinks all 64 zero-amount matured nodes and resets `per_val_unbonding_len` to 0.
        s.claim_unbonded(validator).unwrap();
        assert_eq!(s.per_val_unbonding_len.read(&validator).unwrap(), 0);
        assert_eq!(physical_unbonding_len(&s, validator), 0);

        // Advance to the next epoch (`epoch_start_block >= deactivated_at`) and re-register via `s.stake(...)`.
        val_set.update_epoch(8_000, 1).unwrap();
        s.stake(
            validator,
            validator,
            min_stake,
            (&pk2, radicle_node_id, &sig2),
            (0, &[]),
        )
        .unwrap();
        let state = val_set.validator_state(validator).unwrap();
        assert!(matches!(
            state.lifecycle(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        assert_eq!(s.get_stake(validator).unwrap(), min_stake);
        assert_eq!(state.bonded_stake(), min_stake);
        assert_eq!(state.registry_index(), initial_index);
        assert_eq!(val_set.validator_count().unwrap(), count_before);
    });
}

#[test]
fn unbonding_index_and_claimable_overflows_revert_and_length_underflow_is_fatal() {
    let validator = Address::repeat_byte(0x43);

    // 1. `unbonding_count == u32::MAX` reverts with `UnbondingIndexOverflow`.
    with_staking(|storage, s| {
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), s, validator, U256::from(2_000u64)).unwrap();
        s.unbonding_count.write(u32::MAX).unwrap();

        let err = s.unstake(validator, U256::ONE).unwrap_err();
        let expected = IStaking::UnbondingIndexOverflow {}.abi_encode();
        assert!(matches!(err, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected));
    });

    // 2. `total_claimable` overflow in `claim_unbonded` reverts with `ClaimableAmountOverflow`.
    with_staking(|storage, s| {
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), s, validator, U256::from(2_000u64)).unwrap();
        s.unstake(validator, U256::ONE).unwrap();
        s.unstake(validator, U256::ONE).unwrap();
        s.unbonding_amount.write(&0, U256::MAX).unwrap();
        s.unbonding_amount.write(&1, U256::ONE).unwrap();
        storage.set_block_timestamp(U256::from(4_000u64)).unwrap();

        let err = s.claim_unbonded(validator).unwrap_err();
        let expected = IStaking::ClaimableAmountOverflow {}.abi_encode();
        assert!(matches!(err, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected));
    });

    // 3. Corrupted `per_val_unbonding_len == 0` when unlinking a mature node returns Fatal.
    with_staking(|storage, s| {
        seed_staking_balance(storage.clone(), 2_000);
        stake_registered(storage.clone(), s, validator, U256::from(2_000u64)).unwrap();
        s.unstake(validator, U256::ONE).unwrap();
        s.per_val_unbonding_len.write(&validator, 0).unwrap();
        storage.set_block_timestamp(U256::from(4_000u64)).unwrap();

        let err = s.claim_unbonded(validator).unwrap_err();
        let PrecompileError::FatalWithCause(cause) = err else {
            panic!("expected fatal length underflow, got {err:?}");
        };
        assert!(matches!(
            cause.downcast_ref::<StakingError>(),
            Some(StakingError::UnbondingLengthUnderflow { validator: actual }) if *actual == validator
        ));
    });
}

#[derive(Clone, Debug)]
enum StakingOp {
    Topup { val_idx: usize, units: u64 },
    Unstake { val_idx: usize, units: u64 },
    Slash { val_idx: usize, percent: u64 },
    Claim { val_idx: usize },
    AdvanceTime { delta_secs: u64 },
}

fn op_strategy() -> impl Strategy<Value = StakingOp> {
    prop_oneof![
        (0..3usize, 1_000u64..=50_000u64)
            .prop_map(|(val_idx, units)| StakingOp::Topup { val_idx, units }),
        (0..3usize, 100u64..=20_000u64)
            .prop_map(|(val_idx, units)| StakingOp::Unstake { val_idx, units }),
        (0..3usize, 1u64..=50u64)
            .prop_map(|(val_idx, percent)| StakingOp::Slash { val_idx, percent }),
        (0..3usize).prop_map(|val_idx| StakingOp::Claim { val_idx }),
        (600u64..=4_000u64).prop_map(|delta_secs| StakingOp::AdvanceTime { delta_secs }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn staking_conservation_and_promis_limit_invariants_hold(
        ops in prop::collection::vec(op_strategy(), 1..40)
    ) {
        let validators = [
            Address::repeat_byte(0x51),
            Address::repeat_byte(0x52),
            Address::repeat_byte(0x53),
        ];
        let scale = UNITS_PER_PROTOCOL_MINOR / U256::from(10u64);

        with_staking(|storage, s| {
            s.config_max_stake_percent.write(0).unwrap();
            let min_stake = U256::from(1_000u64) * scale;
            s.config_min_stake.write(min_stake).unwrap();

            let mut cumulative_slashed = U256::ZERO;
            for &v in &validators {
                register_validator(storage.clone(), v);
                let init = U256::from(10_000u64) * scale;
                seed_staking_balance_u256(storage.clone(), init);
                s.topup(v, v, init).unwrap();
            }

            let mut now = 100u64;
            storage.set_block_timestamp(U256::from(now)).unwrap();

            for op in ops {
                match op {
                    StakingOp::Topup { val_idx, units } => {
                        let v = validators[val_idx];
                        let amount = U256::from(units) * scale;
                        let before_escrow = storage.balance(STAKING_ADDRESS).unwrap();
                        storage.set_balance(STAKING_ADDRESS, before_escrow + amount).unwrap();
                        if s.topup(v, v, amount).is_err() {
                            storage.set_balance(STAKING_ADDRESS, before_escrow).unwrap();
                        }
                    }
                    StakingOp::Unstake { val_idx, units } => {
                        let v = validators[val_idx];
                        let amount = U256::from(units) * scale;
                        let _ = s.unstake(v, amount);
                    }
                    StakingOp::Slash { val_idx, percent } => {
                        let v = validators[val_idx];
                        if let Ok(slashed) = s.slash_stake(v, percent) {
                            cumulative_slashed += slashed;
                        }
                    }
                    StakingOp::Claim { val_idx } => {
                        let v = validators[val_idx];
                        let _ = s.claim_unbonded(v);
                    }
                    StakingOp::AdvanceTime { delta_secs } => {
                        now = now.saturating_add(delta_secs);
                        storage.set_block_timestamp(U256::from(now)).unwrap();
                        let _ = s.process_unbonding(now);
                    }
                }

                // Invariant 1: Sum of individual stakes equals `total_staked`.
                let sum_stakes = validators
                    .iter()
                    .map(|v| s.get_stake(*v).unwrap())
                    .fold(U256::ZERO, |acc, x| acc + x);
                prop_assert_eq!(sum_stakes, s.get_total_staked().unwrap());

                // Invariant 2: Escrow balance equals `total_staked + sum(unbonding_amount)`.
                let unbonding_total = active_unbonding_sum(s);
                prop_assert_eq!(
                    storage.balance(STAKING_ADDRESS).unwrap(),
                    sum_stakes + unbonding_total
                );

                // Invariant 3: PromisLimit * 10^12 + rest_minor == cumulative_slashed.
                let pl = PromisLimitContract::new(storage.clone());
                let accounted_burn = pl.promis_limit().unwrap() * UNITS_PER_PROTOCOL_MINOR
                    + pl.rest_minor().unwrap();
                prop_assert_eq!(accounted_burn, cumulative_slashed);

                // Invariant 4: Stored unbonding length matches physical linked list and <= 64.
                for &v in &validators {
                    let stored_len = s.per_val_unbonding_len.read(&v).unwrap();
                    let phys_len = physical_unbonding_len(s, v);
                    prop_assert_eq!(stored_len, phys_len);
                    prop_assert!(stored_len <= MAX_UNBONDING_ENTRIES_PER_VALIDATOR);
                }
            }
            Ok(())
        })
        .unwrap();
    }
}
