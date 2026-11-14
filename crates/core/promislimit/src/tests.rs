use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::schema::PromisLimitContract;

const CHAIN_ID: u64 = 1;

fn with_contract<R>(f: impl FnOnce(&mut PromisLimitContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = PromisLimitContract::new(storage.clone());
        f(&mut contract)
    })
}

#[test]
fn test_initial_state() {
    with_contract(|c| {
        assert_eq!(c.promis_limit().unwrap(), U256::ZERO);
    });
}

#[test]
fn test_set_promis_limit() {
    with_contract(|c| {
        let amount = U256::from(1_000_000u64);
        c.set_promis_limit(amount).unwrap();
        assert_eq!(c.promis_limit().unwrap(), amount);
    });
}

#[test]
fn checked_credit_accumulates() {
    with_contract(|c| {
        c.checked_credit(U256::from(100u64)).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(100u64));

        c.checked_credit(U256::from(250u64)).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(350u64));

        c.checked_credit(U256::from(50u64)).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(400u64));
    });
}

#[test]
fn checked_credit_returns_the_committed_delta() {
    with_contract(|c| {
        c.checked_credit(U256::from(40u64)).unwrap();

        let delta = c
            .checked_credit(U256::from(2u64))
            .expect("promis limit credit");

        assert_eq!(delta.before, U256::from(40u64));
        assert_eq!(delta.credited, U256::from(2u64));
        assert_eq!(delta.after, U256::from(42u64));
        assert_eq!(c.promis_limit().unwrap(), U256::from(42u64));
    });
}

#[test]
fn a_partial_take_serves_what_the_promis_limit_holds() {
    with_contract(|c| {
        c.checked_credit(U256::from(42u64)).unwrap();

        let partial = c
            .checked_take_up_to(U256::from(10u64))
            .expect("partial take");
        assert_eq!(partial.before, U256::from(42u64));
        assert_eq!(partial.taken, U256::from(10u64));
        assert_eq!(partial.after, U256::from(32u64));

        let over = c
            .checked_take_up_to(U256::from(1_000u64))
            .expect("a request above the balance is not an error");
        assert_eq!(over.taken, U256::from(32u64));
        assert_eq!(over.after, U256::ZERO);

        let empty = c.checked_take_up_to(U256::from(5u64)).expect("empty take");
        assert_eq!(empty.taken, U256::ZERO);
        assert_eq!(empty.after, U256::ZERO);
    });
}

#[test]
fn a_strict_take_takes_the_exact_amount_or_nothing() {
    with_contract(|c| {
        c.checked_credit(U256::from(42u64)).unwrap();

        let exact = c
            .checked_take(U256::from(10u64))
            .unwrap()
            .expect("covered take");
        assert_eq!(exact.before, U256::from(42u64));
        assert_eq!(exact.taken, U256::from(10u64));
        assert_eq!(exact.after, U256::from(32u64));

        assert_eq!(c.checked_take(U256::from(33u64)).unwrap(), None);
        assert_eq!(c.promis_limit().unwrap(), U256::from(32u64));

        let all = c
            .checked_take(U256::from(32u64))
            .unwrap()
            .expect("the whole balance");
        assert_eq!(all.after, U256::ZERO);
    });
}

#[test]
fn test_set_overwrites_previous() {
    with_contract(|c| {
        c.set_promis_limit(U256::from(500u64)).unwrap();
        c.set_promis_limit(U256::from(200u64)).unwrap();
        assert_eq!(c.promis_limit().unwrap(), U256::from(200u64));
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_contract(|c| {
        assert_eq!(c.promis_limit.slot(), alloy_primitives::U256::ZERO);
        assert_eq!(c.rest_minor.slot(), alloy_primitives::U256::ONE);
    });
}

// ---------------------------------------------------------------------------
// checked_add overflow rejection
// ---------------------------------------------------------------------------

#[test]
fn an_overflowing_credit_fails_the_block() {
    with_contract(|c| {
        let near_max = U256::MAX - U256::from(10u64);
        c.checked_credit(near_max).unwrap();

        let err = c.checked_credit(U256::from(100u64)).unwrap_err();
        assert!(
            matches!(
                err,
                outbe_primitives::error::PrecompileError::FatalWithCause(_)
            ),
            "{err:?}"
        );

        assert_eq!(c.promis_limit().unwrap(), near_max);
    });
}

#[test]
fn burn_to_promis_limit_accumulates_rest_across_sources_and_rolls_back_on_overflow() {
    use alloy_primitives::Address;
    use alloy_sol_types::{SolCall, SolEvent, SolValue};
    use outbe_system_common::addresses::{
        PROMIS_LIMIT_ADDRESS, STAKING_ADDRESS, SYSTEM_ADDRESS, VOTE_ADDRESS,
    };
    use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

    use crate::{
        precompile::{dispatch, IPromisLimit},
        runtime::BurnSource,
    };

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let two_and_half =
        UNITS_PER_PROTOCOL_MINOR * U256::from(2u64) + (UNITS_PER_PROTOCOL_MINOR / U256::from(2u64));
    let half = UNITS_PER_PROTOCOL_MINOR / U256::from(2u64);
    storage.set_balance(STAKING_ADDRESS, two_and_half + half);
    storage.set_balance(VOTE_ADDRESS, UNITS_PER_PROTOCOL_MINOR);

    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle.clone());
        let mut cumulative_burned_units = U256::ZERO;

        // Zero burn is a no-op and emits nothing.
        assert_eq!(
            contract
                .burn_to_promis_limit(STAKING_ADDRESS, U256::ZERO, BurnSource::Slash)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);

        // Shared function vector (OIP-00309 Test plan):
        // First 2.5 * 10^12 COEN units slash: credits 2 minors, stores 0.5 * 10^12 in rest_minor.
        let credited_first = contract
            .burn_to_promis_limit(STAKING_ADDRESS, two_and_half, BurnSource::Slash)
            .unwrap();
        cumulative_burned_units += two_and_half;
        assert_eq!(credited_first, U256::from(2u64));
        assert_eq!(contract.promis_limit().unwrap(), U256::from(2u64));
        assert_eq!(contract.rest_minor().unwrap(), half);
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), half);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // Second 0.5 * 10^12 COEN units slash: credits 1 minor, leaves rest_minor == 0.
        let credited_second = contract
            .burn_to_promis_limit(STAKING_ADDRESS, half, BurnSource::Slash)
            .unwrap();
        cumulative_burned_units += half;
        assert_eq!(credited_second, U256::ONE);
        assert_eq!(contract.promis_limit().unwrap(), U256::from(3u64));
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // Sub-unit proposal bond burn (0.4) followed by sub-unit slash (0.6) shares rest_minor.
        let point_four = UNITS_PER_PROTOCOL_MINOR * U256::from(4u64) / U256::from(10u64);
        let point_six = UNITS_PER_PROTOCOL_MINOR * U256::from(6u64) / U256::from(10u64);
        handle.set_balance(STAKING_ADDRESS, point_six).unwrap();

        assert_eq!(
            contract
                .burn_to_promis_limit(VOTE_ADDRESS, point_four, BurnSource::ProposalBond)
                .unwrap(),
            U256::ZERO
        );
        cumulative_burned_units += point_four;
        let rest_bytes = dispatch(
            handle.clone(),
            &IPromisLimit::restMinorCall {}.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(U256::abi_decode(&rest_bytes).unwrap(), point_four);

        assert_eq!(
            contract
                .burn_to_promis_limit(STAKING_ADDRESS, point_six, BurnSource::Slash)
                .unwrap(),
            U256::ONE
        );
        cumulative_burned_units += point_six;
        assert_eq!(contract.promis_limit().unwrap(), U256::from(4u64));
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // Mixed sources vector (OIP-00309 Test plan):
        // Executor gives an already-burned BaseFee of 0.9 minor, then Slash sends 0.2 minor.
        // The second call credits 1 minor and leaves 0.1 minor in rest_minor without double-burning.
        let point_nine = UNITS_PER_PROTOCOL_MINOR * U256::from(9u64) / U256::from(10u64);
        let point_two = UNITS_PER_PROTOCOL_MINOR * U256::from(2u64) / U256::from(10u64);
        let point_one = UNITS_PER_PROTOCOL_MINOR / U256::from(10u64);
        handle.set_balance(STAKING_ADDRESS, point_two).unwrap();

        assert_eq!(
            contract
                .burn_to_promis_limit(SYSTEM_ADDRESS, point_nine, BurnSource::BaseFee)
                .unwrap(),
            U256::ZERO
        );
        cumulative_burned_units += point_nine;
        assert_eq!(contract.promis_limit().unwrap(), U256::from(4u64));
        assert_eq!(contract.rest_minor().unwrap(), point_nine);
        assert_eq!(handle.balance(SYSTEM_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        assert_eq!(
            contract
                .burn_to_promis_limit(STAKING_ADDRESS, point_two, BurnSource::Slash)
                .unwrap(),
            U256::ONE
        );
        cumulative_burned_units += point_two;
        assert_eq!(contract.promis_limit().unwrap(), U256::from(5u64));
        assert_eq!(contract.rest_minor().unwrap(), point_one);
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // OIP-00309 §1.8 Control check:
        // promis_limit * UNITS_PER_PROTOCOL_MINOR + rest_minor == sum(all_amounts)
        assert_eq!(
            contract.promis_limit().unwrap() * UNITS_PER_PROTOCOL_MINOR
                + contract.rest_minor().unwrap(),
            cumulative_burned_units
        );

        // Overflow on promis_limit leaves escrow balance and rest_minor untouched.
        contract.set_promis_limit(U256::MAX).unwrap();
        let rest_before = UNITS_PER_PROTOCOL_MINOR / U256::from(5u64);
        contract.rest_minor.write(rest_before).unwrap();
        let vote_balance_before = handle.balance(VOTE_ADDRESS).unwrap();
        let err = contract
            .burn_to_promis_limit(
                VOTE_ADDRESS,
                UNITS_PER_PROTOCOL_MINOR,
                BurnSource::ProposalBond,
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::PromisLimitError::Overflow
            )
            .to_string()
        );
        assert_eq!(contract.promis_limit().unwrap(), U256::MAX);
        assert_eq!(contract.rest_minor().unwrap(), rest_before);
        assert_eq!(handle.balance(VOTE_ADDRESS).unwrap(), vote_balance_before);
    });

    // Only the 5 non-zero escrow-backed burns emit in-tx events; the already-burned BaseFee call does not.
    let logs = storage.get_events(PROMIS_LIMIT_ADDRESS);
    assert_eq!(logs.len(), 5);
    let first_log = &logs[0];
    let decoded = IPromisLimit::NativeBurnedToPromisLimit::decode_raw_log(
        first_log.topics(),
        &first_log.data,
    )
    .unwrap();
    assert_eq!(decoded.source, BurnSource::Slash as u8);
    assert_eq!(decoded.burnedUnit, two_and_half);
}

#[test]
fn only_executor_gives_burned_amount() {
    use alloy_primitives::Bytes;
    use alloy_sol_types::SolError;
    use outbe_primitives::error::PrecompileError;
    use outbe_system_common::addresses::{
        INTEX_FACTORY_ADDRESS, PROMIS_LIMIT_ADDRESS, STAKING_ADDRESS, SYSTEM_ADDRESS, VOTE_ADDRESS,
    };
    use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

    use crate::{precompile::IPromisLimit, runtime::BurnSource};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let seeded_system_balance = UNITS_PER_PROTOCOL_MINOR * U256::from(10u64);
    let seeded_caller_balance = UNITS_PER_PROTOCOL_MINOR * U256::from(10u64);
    storage.set_balance(SYSTEM_ADDRESS, seeded_system_balance);
    storage.set_balance(STAKING_ADDRESS, seeded_caller_balance);
    storage.set_balance(VOTE_ADDRESS, seeded_caller_balance);
    storage.set_balance(INTEX_FACTORY_ADDRESS, seeded_caller_balance);

    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle.clone());

        // 1. Executor (`SYSTEM_ADDRESS`) can submit already-burned BaseFee and BlobFee without any balance debit.
        let base_fee_amount = UNITS_PER_PROTOCOL_MINOR
            + (UNITS_PER_PROTOCOL_MINOR * U256::from(3u64) / U256::from(10u64));
        let blob_fee_amount = UNITS_PER_PROTOCOL_MINOR * U256::from(8u64) / U256::from(10u64);

        assert_eq!(
            contract
                .burn_to_promis_limit(SYSTEM_ADDRESS, base_fee_amount, BurnSource::BaseFee)
                .unwrap(),
            U256::ONE
        );
        assert_eq!(contract.promis_limit().unwrap(), U256::ONE);
        assert_eq!(
            contract.rest_minor().unwrap(),
            UNITS_PER_PROTOCOL_MINOR * U256::from(3u64) / U256::from(10u64)
        );
        assert_eq!(
            handle.balance(SYSTEM_ADDRESS).unwrap(),
            seeded_system_balance
        );
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        assert_eq!(
            contract
                .burn_to_promis_limit(SYSTEM_ADDRESS, blob_fee_amount, BurnSource::BlobFee)
                .unwrap(),
            U256::ONE
        );
        assert_eq!(contract.promis_limit().unwrap(), U256::from(2u64));
        assert_eq!(
            contract.rest_minor().unwrap(),
            UNITS_PER_PROTOCOL_MINOR / U256::from(10u64)
        );
        assert_eq!(
            handle.balance(SYSTEM_ADDRESS).unwrap(),
            seeded_system_balance
        );
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        let limit_snapshot = contract.promis_limit().unwrap();
        let rest_snapshot = contract.rest_minor().unwrap();

        // 2. Non-executor callers attempting to submit BaseFee or BlobFee fail (both zero and non-zero amounts).
        for caller in [STAKING_ADDRESS, VOTE_ADDRESS, INTEX_FACTORY_ADDRESS] {
            for source in [BurnSource::BaseFee, BurnSource::BlobFee] {
                for amount in [U256::ZERO, UNITS_PER_PROTOCOL_MINOR] {
                    let err = contract
                        .burn_to_promis_limit(caller, amount, source)
                        .unwrap_err();
                    let expected_bytes = Bytes::from(
                        IPromisLimit::UnauthorizedBurnCaller {
                            caller,
                            source: source as u8,
                        }
                        .abi_encode(),
                    );
                    assert!(
                        matches!(&err, PrecompileError::RevertBytes(bytes) if bytes == &expected_bytes),
                        "expected UnauthorizedBurnCaller for caller={caller}, source={source:?}, amount={amount}, got {err:?}"
                    );
                    assert_eq!(contract.promis_limit().unwrap(), limit_snapshot);
                    assert_eq!(contract.rest_minor().unwrap(), rest_snapshot);
                    assert_eq!(handle.balance(caller).unwrap(), seeded_caller_balance);
                }
            }
        }

        // 3. `SYSTEM_ADDRESS` attempting to use any escrow-backed source fails (both zero and non-zero amounts).
        let escrow_sources = [
            BurnSource::Slash,
            BurnSource::ProposalBond,
            BurnSource::BlockFee,
            BurnSource::Proceeds,
            BurnSource::LysisUnused,
            BurnSource::NodForfeit,
        ];
        for source in escrow_sources {
            for amount in [U256::ZERO, UNITS_PER_PROTOCOL_MINOR] {
                let err = contract
                    .burn_to_promis_limit(SYSTEM_ADDRESS, amount, source)
                    .unwrap_err();
                let expected_bytes = Bytes::from(
                    IPromisLimit::UnauthorizedBurnCaller {
                        caller: SYSTEM_ADDRESS,
                        source: source as u8,
                    }
                    .abi_encode(),
                );
                assert!(
                    matches!(&err, PrecompileError::RevertBytes(bytes) if *bytes == expected_bytes),
                    "expected UnauthorizedBurnCaller for SYSTEM_ADDRESS with source={source:?}, amount={amount}, got {err:?}"
                );
                assert_eq!(contract.promis_limit().unwrap(), limit_snapshot);
                assert_eq!(contract.rest_minor().unwrap(), rest_snapshot);
                assert_eq!(
                    handle.balance(SYSTEM_ADDRESS).unwrap(),
                    seeded_system_balance
                );
            }
        }
    });

    assert!(storage.get_events(PROMIS_LIMIT_ADDRESS).is_empty());
}
