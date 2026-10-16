use super::*;
use crate::tests::onchain_lysis::{exchange, fixture, CLOCK};

#[test]
fn positive_lysis_rolls_back_every_mutation_and_retries_exactly() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    let timestamp = CLOCK;
    let mut control = fixture();
    control.fail_after_mutation_at(usize::MAX);
    run_start_command(&mut control, 10, timestamp).unwrap();
    let count = control.clear_mutation_failure();
    assert!(count > 20);
    for operation in 0..count {
        let mut provider = fixture();
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(operation);
        assert!(
            run_start_command(&mut provider, 10, timestamp).is_err(),
            "operation {operation}"
        );
        provider.clear_mutation_failure();
        assert_eq!(provider.storage, before, "operation {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            events,
            "operation {operation}"
        );
        run_start_command(&mut provider, 10, timestamp).unwrap();
        assert_eq!(provider.storage, control.storage, "retry {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            control.get_ordered_events(),
            "retry {operation}"
        );
    }
}

#[test]
fn absent_profile_rejects_the_offering_edge_before_any_effect() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0803);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_entry = StorageHandle::enter(&mut provider, |storage| {
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                wwd,
                wwd.start_timestamp(),
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        TributeContract::new(storage).seal_day(wwd).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .lookback_end()
            .read()
            .unwrap()
    });

    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    let error = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, offering_entry, CHAIN_ID),
            storage,
        );
        crate::commands::advance_active_worldwide_days(&ctx)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Revert(_)
    ));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);

    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            MetadosisContract::new(storage).get_wwd_status(wwd).unwrap(),
            status::FORMING
        );
    });
}

#[test]
fn absent_profile_rejects_populated_ready_before_failed_state_or_lysis_effects() {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0804);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = StorageHandle::enter(&mut provider, |storage| {
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::from(1_000));
        MetadosisContract::new(storage)
            .fixture_set_wwd_status(wwd, WwdStatus::Ready)
            .unwrap();
        scheduled
    });

    StorageHandle::enter(&mut provider, |storage| {
        issue_one_tribute(
            &storage,
            FixtureTribute {
                owner: address!("7700000000000000000000000000000000000077"),
                wwd,
                nominal: U256::from(10),
            },
        );
    });
    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    let error = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scheduled, CHAIN_ID),
            storage,
        );
        crate::commands::start_metadosis(&ctx)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Revert(_)
    ));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::READY);
        assert_no_ocomp_job(&storage, wwd);
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(TributeContract::new(storage).total_supply().unwrap(), 1);
    });
}

#[test]
fn metadosis_processes_one_day_per_tick_and_failed_lysis_does_not_block_the_next() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    for older_limit in [1_u64, 1_000_000] {
        let mut provider = fixture();
        let older = outbe_primitives::time::WorldwideDay::new(20261014);
        let newer = outbe_primitives::time::WorldwideDay::new(20261015);
        let timestamp = CLOCK;
        StorageHandle::enter(&mut provider, |storage| {
            let mut contract = MetadosisContract::new(storage.clone());
            contract
                .fixture_create_ready_day(older, U256::from(older_limit), U256::ONE, U256::ONE)
                .unwrap();
            contract.add_active_wwd(older).unwrap();
            issue_one_tribute(
                &storage,
                FixtureTribute {
                    owner: Address::repeat_byte(4),
                    wwd: older,
                    nominal: U256::from(7_654_321),
                },
            );
        });
        run_start_command(&mut provider, 10, timestamp).unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            let contract = MetadosisContract::new(storage.clone());
            assert_eq!(
                contract.get_wwd_status(older).unwrap(),
                if older_limit == 1 {
                    WwdStatus::Failed
                } else {
                    WwdStatus::Completed
                }
            );
            assert_eq!(contract.get_wwd_status(newer).unwrap(), WwdStatus::Ready);
            assert_no_ocomp_job(&storage, older);
        });
        run_start_command(&mut provider, 11, timestamp + 1).unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            let contract = MetadosisContract::new(storage.clone());
            assert_eq!(
                contract.get_wwd_status(newer).unwrap(),
                WwdStatus::Completed
            );
            assert_no_ocomp_job(&storage, newer);
        });
    }
}

#[test]
fn existing_nod_rejects_positive_lysis_without_partial_effects() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    let _nod_enclave = outbe_nod::test_support::scope();
    let mut provider = fixture();
    let day = outbe_primitives::time::WorldwideDay::new(20261015);
    StorageHandle::enter(&mut provider, |storage| {
        let encrypted = outbe_nod::test_support::encrypted_fixture(
            &outbe_nod::NodIssueParams {
                owner: Address::repeat_byte(1),
                worldwide_day: day,
                league_id: 1,
                gratis_load_minor: U256::ONE,
                entry_price_minor: U256::ONE,
                issuance_currency: 840,
                reference_currency: 840,
            },
            CHAIN_ID,
        );
        outbe_nodfactory::api::issue_nod(&storage, &encrypted).unwrap();
    });
    let before = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    assert!(run_start_command(&mut provider, 10, CLOCK).is_err());
    assert_eq!(provider.storage, before);
    assert_eq!(provider.get_ordered_events(), events);
}
