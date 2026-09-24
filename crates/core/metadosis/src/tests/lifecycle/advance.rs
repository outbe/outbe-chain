use super::*;

pub(super) fn run_advance_command(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
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
        crate::commands::advance_active_worldwide_days(&ctx, scope)
    })
}

fn seed_forming_day_for_advance(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> u64 {
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
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
    let probe_scope = ExecutionScope::new();
    let ce_before_probe = probe_scope.ce_work_checkpoint().unwrap();
    probe.fail_after_mutation_at(usize::MAX);
    run_advance_command(&mut probe, &probe_scope, block_number, offering_entry).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 5,
        "normal advance must persist both edges, their events and offering effects"
    );
    assert_eq!(probe_scope.ce_work_checkpoint().unwrap(), ce_before_probe);
    assert_opened_offering_once(&mut probe, wwd, block_number);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let offering_entry = seed_forming_day_for_advance(&mut provider, wwd);
        let scope = ExecutionScope::new();
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();
        let ce_before = scope.ce_work_checkpoint().unwrap();
        provider.fail_after_mutation_at(operation);

        assert!(
            run_advance_command(&mut provider, &scope, block_number, offering_entry).is_err(),
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
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            ce_before,
            "CE work at {operation}"
        );

        run_advance_command(&mut provider, &scope, block_number, offering_entry).unwrap();
        assert_opened_offering_once(&mut provider, wwd, block_number);
    }
}

#[test]
fn cycle_command_restores_all_prior_ce_work_when_a_later_wwd_fails() {
    let first = outbe_primitives::time::WorldwideDay::new(2026_0815);
    let second = outbe_primitives::time::WorldwideDay::new(2026_0816);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let first_offering_end = seed_missed_offering_day(
        &mut provider,
        first,
        U256::from(100),
        U256::ZERO,
        U256::ZERO,
    );
    let second_offering_end = seed_missed_offering_day(
        &mut provider,
        second,
        U256::from(200),
        U256::ZERO,
        U256::ZERO,
    );
    let timestamp = first_offering_end.max(second_offering_end);
    let parent_root = outbe_compressed_entities::sealed_root(B256::repeat_byte(0x81)).unwrap();
    let tree = Arc::new(FailSecondPartitionLookup {
        parent_root,
        partition_root: B256::repeat_byte(0x82),
        calls: AtomicUsize::new(0),
    });
    let scope = ExecutionScope::with_parent_tree(
        tree.clone(),
        outbe_compressed_entities::CeWorkConfig::new(0, 0, u64::MAX),
    );
    StorageHandle::enter(&mut provider, |storage| {
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::ZERO,
                U256::from(4),
            )
            .unwrap();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(parent_root.as_slice()),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();
    let ordered_before = provider.get_ordered_events().to_vec();
    let ce_before = scope.ce_work_checkpoint().unwrap();

    let error = run_advance_command(&mut provider, &scope, 3, timestamp).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::TreeUnavailable(_)
    ));
    assert_eq!(
        tree.calls.load(Ordering::SeqCst),
        2,
        "the first WWD must request retirement before the second lookup fails"
    );
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(provider.get_ordered_events(), ordered_before.as_slice());
    assert_eq!(
        scope.ce_work_checkpoint().unwrap(),
        ce_before,
        "the command checkpoint must remove the first WWD retirement too"
    );
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(first).unwrap(), status::FORMING);
        assert_eq!(metadosis.get_wwd_status(second).unwrap(), status::FORMING);
        assert!(metadosis
            .read_missed_offering_receipt(first)
            .unwrap()
            .is_none());
        assert!(metadosis
            .read_missed_offering_receipt(second)
            .unwrap()
            .is_none());
    });

    run_advance_command(&mut provider, &scope, 3, timestamp).unwrap();
    assert_eq!(tree.calls.load(Ordering::SeqCst), 4);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(first).unwrap(), status::FAILED);
        assert_eq!(metadosis.get_wwd_status(second).unwrap(), status::FAILED);
        assert_eq!(
            metadosis
                .read_missed_offering_receipt(first)
                .unwrap()
                .unwrap()
                .retirement,
            outbe_compressed_entities::RetirementOutcome::Requested
        );
        assert_eq!(
            metadosis
                .read_missed_offering_receipt(second)
                .unwrap()
                .unwrap()
                .retirement,
            outbe_compressed_entities::RetirementOutcome::Requested
        );
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::from(300)
        );
    });
}

/// `advance_active_worldwide_days` (the 12:00 UTC `wwd_advance_noon` Cycle
/// trigger handler) must walk the status machine forward exactly like the
/// midnight path - including the FORMING->OFFERING side effects (tribute day
/// unseal) - but must NOT create a new worldwide day and must NOT settle a
/// READY one; day creation and settlement stay midnight-owned in
/// `start_metadosis`.
#[test]
fn advance_active_worldwide_days_advances_status_without_creating_or_settling() {
    with_storage(|storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
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
        drop(metadosis);

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();
        drop(tribute);

        let advance = |block_number: u64, timestamp: u64| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
                storage.clone(),
            );
            let scope = ExecutionScope::new();
            crate::commands::advance_active_worldwide_days(&ctx, &scope).unwrap();
        };

        // At the offering-entry edge the day opens and the tribute day
        // unseals - offers stop reverting `not in OFFERING status`.
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
        arm_genesis_ocomp(&storage, CHAIN_ID);
        assert_eq!(
            outbe_desis::api::dispatch_auction_brief(
                storage.clone(),
                wwd,
                U256::from(1_u8),
                true,
                scheduled,
                outbe_desis::api::BriefOverflowPolicy::CarryOver,
            )
            .unwrap(),
            outbe_desis::api::AuctionBriefReceipt::Accepted
        );

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scheduled + SECONDS_PER_HOUR, CHAIN_ID),
            storage.clone(),
        );
        with_active_scope(storage.clone(), |scope, parent| {
            assert!(crate::commands::start_metadosis(&ctx, scope, parent).is_err());
        });

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), WwdStatus::Waiting);
        assert!(metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(!metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
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
    let contract_addr = outbe_primitives::addresses::METADOSIS_ADDRESS;

    StorageHandle::enter(&mut storage, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        let timestamp = outbe_primitives::time::WorldwideDay::new(20260302).start_timestamp()
            + 2 * SECONDS_PER_HOUR;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, outbe_primitives::chain::CHAIN_ID),
            storage.clone(),
        );
        crate::emission_sink::apply(&ctx, U256::from(10u64)).unwrap();
        with_active_scope(storage, |scope, parent| {
            crate::commands::start_metadosis(&ctx, scope, parent)
        })
        .unwrap();
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

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        // The day completed and was retired out of the active set into the
        // bounded delete-queue, but stays readable while under the cap.
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis
            .get_active_wwd_by_status(WwdStatus::Completed)
            .unwrap()
            .contains(&wwd));
    });
}
