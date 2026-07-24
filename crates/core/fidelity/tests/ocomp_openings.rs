use alloy_primitives::{address, Address, B256, U256};
use outbe_fidelity::{
    fidelity_count_slot_plan_v1, fidelity_opening_slot_plan_v1, FidelityContract,
    FidelityOpeningPlanError, MAX_OCOMP_COHORTS_PER_OWNER,
};
use outbe_primitives::{
    addresses::FIDELITY_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");
const T0: u64 = 1_700_000_000;

fn slot_word(storage: &StorageHandle<'_>, slot: B256) -> U256 {
    storage
        .sload(FIDELITY_ADDRESS, U256::from_be_bytes(slot.0))
        .unwrap()
}

#[test]
fn fidelity_opening_plan_reads_the_exact_raw_slots_used_by_runtime_semantics() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = FidelityContract::new(storage.clone());
        contract.cohort_in(OWNER, U256::from(100), T0).unwrap();
        contract.cohort_in(OWNER, U256::from(50), T0 + 1).unwrap();
        contract.cohort_out(OWNER, U256::from(30), T0 + 2).unwrap();

        let counts = fidelity_count_slot_plan_v1(OWNER);
        assert_eq!(
            counts.slots().map(|slot| slot_word(&storage, slot)),
            [U256::from(T0), U256::from(T0), U256::from(2), U256::from(1),]
        );

        let plan = fidelity_opening_slot_plan_v1(OWNER, 2, 1).unwrap();
        let values = plan
            .slots
            .iter()
            .copied()
            .map(|slot| slot_word(&storage, slot))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                U256::from(T0),
                U256::from(T0),
                U256::from(2),
                U256::from(100),
                U256::from(T0),
                U256::from(20),
                U256::from(T0 + 1),
                U256::from(1),
                U256::from(30),
                U256::from(T0 + 1),
                U256::from(T0 + 2),
            ]
        );
    });
}

#[test]
fn fidelity_opening_plan_accepts_64_and_rejects_65_before_detail_allocation() {
    let at_cap = fidelity_opening_slot_plan_v1(OWNER, MAX_OCOMP_COHORTS_PER_OWNER, 0).unwrap();
    assert_eq!(at_cap.slots.len(), 4 + 2 * 64);
    assert_eq!(
        fidelity_opening_slot_plan_v1(OWNER, MAX_OCOMP_COHORTS_PER_OWNER + 1, 0),
        Err(FidelityOpeningPlanError::ActiveCountExceedsCap {
            actual: 65,
            cap: 64,
        })
    );
    assert_eq!(
        fidelity_opening_slot_plan_v1(OWNER, 0, MAX_OCOMP_COHORTS_PER_OWNER + 1),
        Err(FidelityOpeningPlanError::SoldCountExceedsCap {
            actual: 65,
            cap: 64,
        })
    );
}
