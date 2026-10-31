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
fn checked_credit_rejects_overflow() {
    with_contract(|c| {
        let near_max = U256::MAX - U256::from(10u64);
        c.checked_credit(near_max).unwrap();

        let err = c.checked_credit(U256::from(100u64)).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::PromisLimitError::Overflow
            )
            .to_string()
        );

        assert_eq!(c.promis_limit().unwrap(), near_max);
    });
}

#[test]
fn burn_to_promis_limit_accumulates_rest_across_sources_and_rolls_back_on_overflow() {
    use alloy_primitives::Address;
    use alloy_sol_types::{SolCall, SolEvent, SolValue};
    use outbe_primitives::{
        addresses::{PROMIS_LIMIT_ADDRESS, STAKING_ADDRESS, VOTE_ADDRESS},
        units::UNITS_PER_PROTOCOL_MINOR,
    };

    use crate::{
        precompile::{dispatch, IPromisLimit},
        runtime::BurnSource,
    };

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let one_and_half = UNITS_PER_PROTOCOL_MINOR + (UNITS_PER_PROTOCOL_MINOR / U256::from(2u64));
    storage.set_balance(STAKING_ADDRESS, one_and_half * U256::from(2u64));
    storage.set_balance(VOTE_ADDRESS, UNITS_PER_PROTOCOL_MINOR);

    StorageHandle::enter(&mut storage, |handle| {
        let mut contract = PromisLimitContract::new(handle.clone());

        // Zero burn is a no-op and emits nothing.
        assert_eq!(
            contract
                .burn_to_promis_limit(STAKING_ADDRESS, U256::ZERO, BurnSource::Slash)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);

        // First 1.5 protocol-unit slash: credits 1 unit, stores 0.5 in rest_minor.
        let credited_first = contract
            .burn_to_promis_limit(STAKING_ADDRESS, one_and_half, BurnSource::Slash)
            .unwrap();
        assert_eq!(credited_first, U256::ONE);
        assert_eq!(contract.promis_limit().unwrap(), U256::ONE);
        assert_eq!(
            contract.rest_minor().unwrap(),
            UNITS_PER_PROTOCOL_MINOR / U256::from(2u64)
        );
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), one_and_half);
        assert_eq!(handle.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // Second 1.5 protocol-unit slash: credits 2 units, leaves rest_minor == 0.
        let credited_second = contract
            .burn_to_promis_limit(STAKING_ADDRESS, one_and_half, BurnSource::Slash)
            .unwrap();
        assert_eq!(credited_second, U256::from(2u64));
        assert_eq!(contract.promis_limit().unwrap(), U256::from(3u64));
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);
        assert_eq!(handle.balance(STAKING_ADDRESS).unwrap(), U256::ZERO);

        // Sub-unit proposal bond burn (0.4) followed by sub-unit slash (0.6) shares rest_minor.
        let point_four = UNITS_PER_PROTOCOL_MINOR * U256::from(4u64) / U256::from(10u64);
        let point_six = UNITS_PER_PROTOCOL_MINOR * U256::from(6u64) / U256::from(10u64);
        handle.set_balance(STAKING_ADDRESS, point_six).unwrap();

        assert_eq!(
            contract
                .burn_to_promis_limit(VOTE_ADDRESS, point_four, BurnSource::ProposalBond,)
                .unwrap(),
            U256::ZERO
        );
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
        assert_eq!(contract.promis_limit().unwrap(), U256::from(4u64));
        assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);

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

    let logs = storage.get_events(PROMIS_LIMIT_ADDRESS);
    assert_eq!(logs.len(), 4);
    let first_log = &logs[0];
    let decoded = IPromisLimit::NativeBurnedToPromisLimit::decode_raw_log(
        first_log.topics(),
        &first_log.data,
    )
    .unwrap();
    assert_eq!(decoded.source, BurnSource::Slash as u8);
    assert_eq!(decoded.burnedUnit, one_and_half);
}
