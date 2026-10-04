//! `api::issue_gem` stamps the Genesis privilege and rolls the gem back if that
//! write fails. `runtime::issue_gem` is the inner writer and does not stamp.

use super::*;

fn genesis_load() -> U256 {
    U256::from(10u64) * six_decimal_unit()
}

fn issue_through_api(storage: &StorageHandle) -> outbe_primitives::error::Result<U256> {
    crate::api::issue_gem(
        storage,
        ALICE,
        GemTypes::Genesis,
        genesis_load(),
        840,
        840,
        U256::from(2u64) * six_decimal_unit(),
    )
}

#[test]
fn api_issue_gem_stamps_genesis_and_runtime_issue_gem_does_not() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let stamped = issue_through_api(storage).unwrap();
        let stamped_item = gem_api::get_gem(storage, stamped).unwrap().unwrap();
        assert!(gem_api::is_qualified(storage, &stamped_item).unwrap());
        assert_eq!(
            GemContract::new(storage.clone())
                .issued_before_first_wwd
                .read(&stamped)
                .unwrap(),
            1
        );

        let plain = runtime::issue_gem(
            storage,
            BOB,
            GemTypes::Genesis,
            genesis_load(),
            840,
            840,
            rate,
        )
        .unwrap();
        let plain_item = gem_api::get_gem(storage, plain).unwrap().unwrap();
        assert!(!gem_api::is_qualified(storage, &plain_item).unwrap());
        assert_eq!(
            GemContract::new(storage.clone())
                .issued_before_first_wwd
                .read(&plain)
                .unwrap(),
            0
        );
    });
}

#[test]
fn a_failed_genesis_flag_write_rolls_the_new_gem_back() {
    let rate = Some(U256::from(2u64) * six_decimal_unit());
    let mut probe = test_storage(rate);
    probe.fail_after_mutation_at(usize::MAX);
    probe.enter(|storage| issue_through_api(&storage)).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count > 1,
        "genesis issuance persists the gem and then the privilege bit"
    );

    for operation in 0..mutation_count {
        let mut provider = test_storage(rate);
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        provider.fail_after_mutation_at(operation);
        let failed = provider.enter(|storage| issue_through_api(&storage));
        assert!(
            failed.is_err(),
            "mutation {operation} unexpectedly succeeded"
        );
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        provider.clear_mutation_failure();

        let gem_id = provider
            .enter(|storage| issue_through_api(&storage))
            .unwrap();
        provider.enter(|storage| {
            let item = gem_api::get_gem(&storage, gem_id).unwrap().unwrap();
            assert!(gem_api::is_qualified(&storage, &item).unwrap());
        });
    }
}

#[test]
fn a_retained_day_with_a_zero_creation_count_does_not_stamp_or_backfill() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let historical = runtime::issue_gem(
            storage,
            ALICE,
            GemTypes::Genesis,
            genesis_load(),
            840,
            840,
            rate,
        )
        .unwrap();
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.issued_before_first_wwd.read(&historical).unwrap(), 0);

        // Retained day, creation slot never written. That is the pre-slot chain.
        outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(
            storage.clone(),
            &[WorldwideDay::new(20_240_101)],
        )
        .unwrap();
        assert_eq!(
            outbe_metadosis::api::worldwide_days_created(storage.clone()).unwrap(),
            0
        );
        assert!(outbe_metadosis::api::has_created_worldwide_day(storage.clone()).unwrap());
        assert_eq!(gem.issued_before_first_wwd.read(&historical).unwrap(), 0);
        let historical_item = gem_api::get_gem(storage, historical).unwrap().unwrap();
        assert!(!gem_api::is_qualified(storage, &historical_item).unwrap());

        let fresh = issue_through_api(storage).unwrap();
        assert_eq!(gem.issued_before_first_wwd.read(&fresh).unwrap(), 0);
        assert_eq!(gem.issued_before_first_wwd.read(&historical).unwrap(), 0);
        let fresh_item = gem_api::get_gem(storage, fresh).unwrap().unwrap();
        assert!(!gem_api::is_qualified(storage, &fresh_item).unwrap());
    });
}
