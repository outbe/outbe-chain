use alloy_primitives::address;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::FidelityContract;

fn with_contract<R>(f: impl FnOnce(&mut FidelityContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = FidelityContract::new(storage.clone());
        f(&mut contract)
    })
}

#[test]
fn test_default_fidelity_index_is_one() {
    with_contract(|contract| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        assert_eq!(contract.get_fidelity_index(alice).unwrap(), 1);
    });
}

#[test]
fn test_set_and_get_fidelity_index() {
    with_contract(|contract| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        contract.set_fidelity_index(alice, 7).unwrap();
        assert_eq!(contract.get_fidelity_index(alice).unwrap(), 7);
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_contract(|contract| {
        assert_eq!(
            contract.fidelity_indices.base_slot(),
            alloy_primitives::U256::ZERO
        );
    });
}

// u32::MAX upper bound on set_fidelity_index

#[test]
fn test_set_fidelity_index_accepts_u32_max() {
    with_contract(|contract| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        contract.set_fidelity_index(alice, u32::MAX as u64).unwrap();
        assert_eq!(contract.get_fidelity_index(alice).unwrap(), u32::MAX as u64);
    });
}

#[test]
fn test_set_fidelity_index_rejects_above_u32_max() {
    with_contract(|contract| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let err = contract
            .set_fidelity_index(alice, u32::MAX as u64 + 1)
            .unwrap_err();
        assert!(err.to_string().contains("exceeds u32::MAX"));
        // write was not applied
        assert_eq!(contract.get_fidelity_index(alice).unwrap(), 1);
    });
}
