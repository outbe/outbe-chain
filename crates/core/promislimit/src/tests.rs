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
        assert_eq!(c.get_total_unallocated().unwrap(), U256::ZERO);
    });
}

#[test]
fn test_set_total_unallocated() {
    with_contract(|c| {
        let amount = U256::from(1_000_000u64);
        c.set_total_unallocated(amount).unwrap();
        assert_eq!(c.get_total_unallocated().unwrap(), amount);
    });
}

#[test]
fn test_add_to_total_unallocated() {
    with_contract(|c| {
        c.add_to_total_unallocated(U256::from(100u64)).unwrap();
        assert_eq!(c.get_total_unallocated().unwrap(), U256::from(100u64));

        c.add_to_total_unallocated(U256::from(250u64)).unwrap();
        assert_eq!(c.get_total_unallocated().unwrap(), U256::from(350u64));

        c.add_to_total_unallocated(U256::from(50u64)).unwrap();
        assert_eq!(c.get_total_unallocated().unwrap(), U256::from(400u64));
    });
}

#[test]
fn test_set_overwrites_previous() {
    with_contract(|c| {
        c.set_total_unallocated(U256::from(500u64)).unwrap();
        c.set_total_unallocated(U256::from(200u64)).unwrap();
        assert_eq!(c.get_total_unallocated().unwrap(), U256::from(200u64));
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_contract(|c| {
        assert_eq!(c.total_unallocated.slot(), alloy_primitives::U256::ZERO);
    });
}

// ---------------------------------------------------------------------------
// checked_add overflow rejection
// ---------------------------------------------------------------------------

#[test]
fn test_add_to_total_unallocated_rejects_overflow() {
    with_contract(|c| {
        let near_max = U256::MAX - U256::from(10u64);
        c.add_to_total_unallocated(near_max).unwrap();

        let err = c.add_to_total_unallocated(U256::from(100u64)).unwrap_err();
        assert!(err.to_string().contains("overflow"));

        assert_eq!(c.get_total_unallocated().unwrap(), near_max);
    });
}
