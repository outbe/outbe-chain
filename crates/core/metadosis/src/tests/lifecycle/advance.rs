use super::*;

pub(super) fn run_advance_command(
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
        crate::commands::advance_active_worldwide_days(&ctx)
    })
}

fn seed_forming_day_for_advance(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> u64 {
    StorageHandle::enter(provider, |storage| {
        install_test_enclaves();
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
        metadosis.set_metadosis_limit(wwd, U256::ONE).unwrap();
        TributeContract::new(storage).seal_day(wwd).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .lookback_end()
            .read()
            .unwrap()
    })
}

fn assert_opened_offering_once(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    block_number: u64,
) {
    StorageHandle::enter(provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);
        assert_eq!(metadosis.active_wwd.read_all().unwrap(), vec![wwd]);
        assert!(!TributeContract::new(storage).is_day_sealed(wwd).unwrap());
    });
    assert_eq!(
        provider_status_events(provider, wwd),
        vec![
            (status::FORMING, status::LOOKBACK_DELAY, block_number),
            (status::LOOKBACK_DELAY, status::OFFERING, block_number),
        ]
    );
}

#[test]
fn normal_advance_command_rolls_back_every_mutation_and_retries_with_ordered_edges() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0811);
    let block_number = 2;

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    let offering_entry = seed_forming_day_for_advance(&mut probe, wwd);

    probe.fail_after_mutation_at(usize::MAX);
    run_advance_command(&mut probe, block_number, offering_entry).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 5,
        "normal advance must persist both edges, their events and offering effects"
    );

    assert_opened_offering_once(&mut probe, wwd, block_number);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let offering_entry = seed_forming_day_for_advance(&mut provider, wwd);

        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();

        provider.fail_after_mutation_at(operation);

        assert!(
            run_advance_command(&mut provider, block_number, offering_entry).is_err(),
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

        run_advance_command(&mut provider, block_number, offering_entry).unwrap();
        assert_opened_offering_once(&mut provider, wwd, block_number);
    }
}

/// `advance_active_worldwide_days` is the 12:00 UTC `wwd_advance_noon` Cycle
/// trigger handler. It must walk the status machine forward exactly like the
/// midnight path, including the FORMING->OFFERING side effects (tribute day
/// unseal). But it must NOT create a new worldwide day and must NOT settle a
/// READY one. Day creation and settlement stay midnight-owned in
/// `start_metadosis`.
#[test]
fn advance_active_worldwide_days_advances_status_without_creating_or_settling() {
    with_storage(|storage| {
        install_test_enclaves();
        let wwd = outbe_primitives::time::WorldwideDay::new(20260302u32);
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let offering_entry = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let offering_end = offering_entry + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let scheduled = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                wwd,
                forming_start,
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        metadosis.set_metadosis_limit(wwd, U256::ONE).unwrap();
        drop(metadosis);

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();
        drop(tribute);

        let advance = |block_number: u64, timestamp: u64| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
                storage.clone(),
            );

            crate::commands::advance_active_worldwide_days(&ctx).unwrap();
        };

        // At the offering-entry edge the day opens and the tribute day
        // unseals. Offers stop reverting `not in OFFERING status`.
        advance(2, offering_entry);
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);
        let tribute = TributeContract::new(storage.clone());
        assert!(!tribute.is_day_sealed(wwd).unwrap());

        // Advancing did not create any other worldwide day.
        let active = metadosis.active_wwd.read_all().unwrap();
        assert_eq!(active, vec![wwd], "advance must not create worldwide days");
        drop(metadosis);

        // Past scheduled-process time the walk parks the day at READY and
        // leaves it active: settlement belongs to `start_metadosis` only.
        advance(3, scheduled);
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::READY);
        assert_eq!(
            metadosis.active_wwd.read_all().unwrap(),
            vec![wwd],
            "advance must not settle or retire a READY day"
        );
    });
}

#[test]
fn technical_desis_refusal_rolls_back_the_metadosis_cycle_command() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0322);
        let day_limit = U256::from(777_u64);
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, day_limit);
        install_test_enclaves();
        assert_eq!(
            outbe_desis::api::dispatch_auction_brief(
                storage.clone(),
                outbe_desis::api::AuctionBrief {
                    worldwide_day: wwd,
                    desis_limit_minor: U256::from(1_u8),
                    is_green: true
                },
                scheduled,
                outbe_desis::api::BriefOverflowPolicy::ReturnToPromisLimit
            )
            .unwrap(),
            outbe_desis::api::AuctionBriefReceipt::Accepted
        );

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scheduled + SECONDS_PER_HOUR, CHAIN_ID),
            storage.clone(),
        );
        assert!(crate::commands::start_metadosis(&ctx).is_err());

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), WwdStatus::Waiting);
        assert!(metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(!metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            U256::ZERO
        );
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.pending_desis_limit_minor.read(&wwd).unwrap(),
            U256::from(1_u8)
        );
        assert_eq!(desis.sched_active_count.read().unwrap(), 1);
    });
}

#[test]
fn test_events_emitted_for_accumulation_and_lifecycle() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_metadosis_mutation_frames(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        2,
    );
    outbe_fidelity::enclave_client::test_enclave::install();
    outbe_tribute::enclave_client::test_enclave::install();
    let contract_addr = outbe_primitives::addresses::METADOSIS_ADDRESS;

    StorageHandle::enter(&mut storage, |storage| {
        install_test_enclaves();
        let timestamp = outbe_primitives::time::WorldwideDay::new(20260302).start_timestamp()
            + 2 * SECONDS_PER_HOUR;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, outbe_primitives::chain::CHAIN_ID),
            storage.clone(),
        );
        crate::emission_sink::apply(&ctx, U256::from(10u64)).unwrap();
        crate::commands::start_metadosis(&ctx).unwrap();
    });

    let events = storage.get_events(contract_addr);
    assert!(
        events.len() >= 2,
        "expected accumulation + lifecycle events"
    );
}

#[test]
fn test_terminal_day_leaves_active_set() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(20260315u32);
        let forming_start = wwd.start_timestamp();
        let scheduled = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
            + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
            + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                wwd,
                forming_start,
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        metadosis.set_wwd_day_type(wwd, WwdDayType::Red).unwrap();
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
            .unwrap();
        metadosis
            .set_metadosis_limit(wwd, U256::from(777u64))
            .unwrap();

        TributeContract::new(storage.clone()).seal_day(wwd).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        // The day completed. Metadosis retired it out of the active set into the
        // bounded delete-queue, but it stays readable while under the cap.
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis
            .get_active_wwd_by_status(WwdStatus::Completed)
            .unwrap()
            .contains(&wwd));
    });
}
