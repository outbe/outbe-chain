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
        assert!(err.to_string().contains("overflow"));

        assert_eq!(c.promis_limit().unwrap(), near_max);
    });
}
