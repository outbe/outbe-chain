use super::*;

fn run_init_genesis_command(
    provider: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    provider.set_block_number(block_number);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
            storage,
        );
        crate::commands::init_genesis_day(&ctx)
    })
}

fn seed_genesis_profile(provider: &mut HashMapStorageProvider) {
    StorageHandle::enter(provider, |storage| arm_genesis_ocomp(&storage, CHAIN_ID));
}

fn assert_genesis_created_once(
    provider: &mut HashMapStorageProvider,
    timestamp: u64,
) -> outbe_primitives::time::WorldwideDay {
    let expected = outbe_primitives::time::WorldwideDay::from_timestamp(timestamp);
    StorageHandle::enter(provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.active_wwd.read_all().unwrap(), vec![expected]);
        assert_eq!(metadosis.get_wwd_status(expected).unwrap(), status::FORMING);
        assert!(TributeContract::new(storage)
            .is_day_sealed(expected)
            .unwrap());
    });
    let started = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::WorldwideDayStarted::decode_log(event).ok())
        .filter(|event| event.data.worldwideDay == expected.value())
        .count();
    assert_eq!(started, 1);
    expected
}

#[test]
fn genesis_creation_command_rolls_back_every_mutation_and_retries_exactly_once() {
    let timestamp = outbe_primitives::time::WorldwideDay::new(2026_0810).to_timestamp_utc()
        + 2 * SECONDS_PER_HOUR;

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    seed_genesis_profile(&mut probe);
    probe.fail_after_mutation_at(usize::MAX);
    run_init_genesis_command(&mut probe, 1, timestamp).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 4,
        "creation must persist the WWD, membership, Tribute seal and event"
    );
    let expected = assert_genesis_created_once(&mut probe, timestamp);
    assert_eq!(
        provider_status_events(&probe, expected),
        Vec::<(u8, u8, u64)>::new()
    );

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_genesis_profile(&mut provider);
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(operation);

        assert!(
            run_init_genesis_command(&mut provider, 1, timestamp).is_err(),
            "mutation {operation} unexpectedly succeeded"
        );
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            ordered_before.as_slice(),
            "ordered events at {operation}"
        );

        run_init_genesis_command(&mut provider, 1, timestamp).unwrap();
        assert_eq!(
            assert_genesis_created_once(&mut provider, timestamp),
            expected
        );
    }
}

pub(super) fn provider_status_events(
    provider: &HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> Vec<(u8, u8, u64)> {
    provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::WorldwideDayStatusChange::decode_log(event).ok())
        .filter(|event| event.data.worldwideDay == wwd.value())
        .map(|event| {
            (
                event.data.oldStatus,
                event.data.newStatus,
                event.data.blockNumber,
            )
        })
        .collect()
}

#[test]
fn test_cold_start_creates_only_current_utc_plus_14_day() {
    with_storage(|storage| {
        let timestamp = outbe_primitives::time::WorldwideDay::new(20260302).start_timestamp()
            + 2 * SECONDS_PER_HOUR;
        run_begin_block(storage.clone(), 1, timestamp);

        let metadosis = MetadosisContract::new(storage.clone());
        let active = metadosis.active_wwd.read_all().unwrap();
        assert_eq!(active, vec![20260302u32.into()]);
        assert_eq!(
            metadosis.get_bootstrap_end_time().unwrap(),
            timestamp + BOOTSTRAP_DURATION_HOURS * SECONDS_PER_HOUR
        );

        let tribute = TributeContract::new(storage);
        assert!(tribute.is_day_sealed(20260302u32.into()).unwrap());
    });
}

#[test]
fn genesis_day_uses_the_canonical_utc_plus_14_boundary() {
    let utc_midnight = crate::runtime::date_key_to_timestamp(20260302);

    for (timestamp, expected) in [
        (utc_midnight + 9 * SECONDS_PER_HOUR + 59 * 60, 20260302),
        (utc_midnight + 10 * SECONDS_PER_HOUR, 20260303),
    ] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_genesis_profile(&mut provider);
        run_init_genesis_command(&mut provider, 1, timestamp).unwrap();

        let created = assert_genesis_created_once(&mut provider, timestamp);
        assert_eq!(created, outbe_primitives::time::WorldwideDay::new(expected));
        let constants = outbe_chain_constants::GenesisProtocolParametersV1::default();
        StorageHandle::enter(&mut provider, |storage| {
            let metadosis = MetadosisContract::new(storage);
            let day = metadosis.worldwide_days.entry(created);
            let forming_start = created.start_timestamp();
            let forming_end = forming_start + constants.metadosis_forming_period_seconds;
            let lookback_end = forming_end + constants.metadosis_lookback_delay_seconds;
            let offering_end = lookback_end + constants.metadosis_offering_period_seconds;
            assert_eq!(day.forming_start().read().unwrap(), forming_start);
            assert_eq!(day.forming_end().read().unwrap(), forming_end);
            assert_eq!(day.lookback_end().read().unwrap(), lookback_end);
            assert_eq!(day.offering_end().read().unwrap(), offering_end);
            assert_eq!(
                day.scheduled_process_time().read().unwrap(),
                offering_end + constants.metadosis_waiting_period_seconds
            );
        });
    }
}

#[test]
fn test_cold_start_uses_genesis_default_schedule_independent_of_chain_id() {
    with_storage(|storage| {
        let timestamp = outbe_primitives::time::WorldwideDay::new(20260302).start_timestamp()
            + 2 * SECONDS_PER_HOUR;
        run_begin_block_with_chain_id(storage.clone(), 1, timestamp, CHAIN_ID);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_bootstrap_end_time().unwrap(),
            timestamp + BOOTSTRAP_DURATION_HOURS * SECONDS_PER_HOUR
        );

        let active = metadosis.active_wwd.read_all().unwrap();
        assert_eq!(active, vec![20260302u32.into()]);

        let wwd = 20260302u32;
        let forming_start = outbe_primitives::time::WorldwideDay::new(wwd).start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let expected_lookback_end = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let expected_offering_end =
            expected_lookback_end + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;

        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd.into())
                .lookback_end()
                .read()
                .unwrap(),
            expected_lookback_end
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd.into())
                .offering_end()
                .read()
                .unwrap(),
            expected_offering_end
        );
    });
}
