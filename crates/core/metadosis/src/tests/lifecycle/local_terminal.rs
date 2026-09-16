use super::*;

fn issue_one_tribute_and_run_metadosis(
    storage: &StorageHandle,
    wwd: outbe_primitives::time::WorldwideDay,
    nominal: U256,
    block_number: u64,
    timestamp: u64,
) {
    let owner = address!("7400000000000000000000000000000000000074");
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
        storage.clone(),
    );
    with_active_scope(storage.clone(), |scope, parent| {
        issue_one_tribute_in_scope(storage, scope, parent, owner, wwd, nominal);
        crate::commands::start_metadosis(&ctx, scope, parent).unwrap();
    });
}

fn seed_local_terminal_fixture(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    day_type: u8,
    day_limit: U256,
    tribute_count: u32,
    tribute_nominal: U256,
) -> u64 {
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        let scheduled = create_waiting_day(&storage, wwd, day_type, day_limit);
        super::arm_reference_price(&storage, scheduled);
        let mut tribute = TributeContract::new(storage);
        tribute.initialize_fresh_ocomp_profile().unwrap();
        tribute
            .total_supply
            .write(u64::from(tribute_count))
            .unwrap();
        tribute
            .day_totals
            .create(&outbe_tribute::DayTotals {
                worldwide_day: wwd,
                initialized: true,
                tribute_count,
                tribute_nominal_amount: tribute_nominal,
                is_sealed: true,
            })
            .unwrap();
        let admission = tribute.pre_admission_projection(wwd).unwrap();
        assert!(admission.profile_ready);
        assert!(
            !admission.is_sealed,
            "local terminal classification precedes OCOMP pre-admission sealing"
        );
        assert_eq!(admission.tribute_count, tribute_count);
        assert_eq!(admission.tribute_nominal_amount, tribute_nominal);
        scheduled
    })
}

fn assert_local_terminal_completed(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    expected_promis: U256,
) {
    assert_local_terminal_outcome(provider, wwd, status::COMPLETED, expected_promis);
}

fn assert_local_terminal_outcome(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    expected_status: u8,
    expected_promis: U256,
) {
    StorageHandle::enter(provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), expected_status);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            expected_promis
        );
    });
}

fn exercise_local_terminal_fault_matrix(
    wwd: outbe_primitives::time::WorldwideDay,
    day_type: u8,
    day_limit: U256,
    tribute_count: u32,
    tribute_nominal: U256,
    expected_status: u8,
    expected_promis: U256,
) {
    let block_number = 2;

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut probe,
        wwd,
        day_type,
        day_limit,
        tribute_count,
        tribute_nominal,
    );
    let (probe_scope, probe_parent) = begin_persistent_active_scope(&mut probe);
    let ce_before_probe = probe_scope.ce_work_checkpoint().unwrap();
    probe.fail_after_mutation_at(usize::MAX);
    run_start_command(
        &mut probe,
        &probe_scope,
        &probe_parent,
        block_number,
        scheduled,
    )
    .unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 7,
        "local terminal command must include transition, domain effects, retirement and events"
    );
    assert_eq!(probe_scope.ce_work_checkpoint().unwrap(), ce_before_probe);
    assert_local_terminal_outcome(&mut probe, wwd, expected_status, expected_promis);
    let clean_storage = probe.storage.clone();
    let clean_events = probe.events.clone();
    let clean_ordered_events = probe.get_ordered_events().to_vec();
    let clean_ce_work = probe_scope.ce_work_checkpoint().unwrap();
    end_persistent_active_scope(&mut probe, &probe_scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let scheduled = seed_local_terminal_fixture(
            &mut provider,
            wwd,
            day_type,
            day_limit,
            tribute_count,
            tribute_nominal,
        );
        let (scope, parent) = begin_persistent_active_scope(&mut provider);
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_before = provider.get_ordered_events().to_vec();
        let ce_before = scope.ce_work_checkpoint().unwrap();
        provider.fail_after_mutation_at(operation);

        assert!(
            run_start_command(&mut provider, &scope, &parent, block_number, scheduled).is_err(),
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

        run_start_command(&mut provider, &scope, &parent, block_number, scheduled).unwrap();
        assert_local_terminal_outcome(&mut provider, wwd, expected_status, expected_promis);
        assert_eq!(
            provider.storage, clean_storage,
            "local terminal retry storage diverged at {operation}"
        );
        assert_eq!(
            provider.events, clean_events,
            "local terminal retry events diverged at {operation}"
        );
        assert_eq!(
            provider.get_ordered_events(),
            clean_ordered_events.as_slice(),
            "local terminal retry ordered events diverged at {operation}"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            clean_ce_work,
            "local terminal retry CE work diverged at {operation}"
        );
        end_persistent_active_scope(&mut provider, &scope);
    }
}

#[test]
fn empty_tribute_day_command_rolls_back_every_mutation_and_ce_work_then_retries() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0812),
        day_type::RED,
        U256::from(777),
        0,
        U256::ZERO,
        status::COMPLETED,
        U256::from(777),
    );
}

#[test]
fn zero_gratis_command_rolls_back_every_mutation_and_ce_work_then_retries() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0813),
        day_type::RED,
        U256::from(2),
        1,
        U256::from(1_000),
        status::COMPLETED,
        U256::from(2),
    );
}

#[test]
fn zero_day_limit_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0820),
        day_type::GREEN,
        U256::ZERO,
        1,
        U256::from(1_000),
        status::FAILED,
        U256::ZERO,
    );
}

#[test]
fn unknown_day_type_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0821),
        day_type::UNKNOWN,
        U256::from(777),
        1,
        U256::from(1_000),
        status::FAILED,
        U256::from(777),
    );
}

#[test]
fn green_empty_tribute_day_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0822),
        day_type::GREEN,
        U256::from(777),
        0,
        U256::ZERO,
        status::COMPLETED,
        U256::from(777),
    );
}

#[test]
fn green_zero_gratis_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        outbe_primitives::time::WorldwideDay::new(2026_0823),
        day_type::GREEN,
        U256::from(2),
        1,
        U256::ONE,
        status::COMPLETED,
        U256::ONE,
    );
}

#[test]
fn zero_gratis_completes_with_a_present_parent_partition_without_retiring_input() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0818);
    let day_limit = U256::from(2);
    let nominal = U256::from(1_000);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled =
        seed_local_terminal_fixture(&mut provider, wwd, day_type::RED, day_limit, 1, nominal);
    let parent_root = outbe_compressed_entities::sealed_root(B256::repeat_byte(0x86)).unwrap();
    let tree = Arc::new(FailSecondPartitionLookup {
        parent_root,
        partition_root: B256::repeat_byte(0x87),
        calls: AtomicUsize::new(0),
    });
    let scope = ExecutionScope::with_parent_tree(
        tree.clone(),
        outbe_compressed_entities::CeWorkConfig::new(0, 0, u64::MAX),
    );
    let parent = TestParent::empty();
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
    let ce_before = scope.ce_work_checkpoint().unwrap();

    run_start_command(&mut provider, &scope, &parent, 2, scheduled).unwrap();

    assert_eq!(
        tree.calls.load(Ordering::SeqCst),
        0,
        "populated zero-gratis input remains available and is not retired"
    );
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    assert_local_terminal_completed(&mut provider, wwd, day_limit);
    StorageHandle::enter(&mut provider, |storage| {
        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_amount, nominal);
        assert_no_ocomp_job(&storage, wwd);
    });
}

#[test]
fn empty_tribute_day_restores_outer_ce_checkpoint_after_late_parent_failure_then_retries() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0814);
    let day_limit = U256::from(777);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled =
        seed_local_terminal_fixture(&mut provider, wwd, day_type::RED, day_limit, 0, U256::ZERO);
    let parent_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let tree = Arc::new(FailOncePartitionLookup {
        parent_root,
        calls: AtomicUsize::new(0),
    });
    let scope = ExecutionScope::with_parent_tree(
        tree.clone(),
        outbe_compressed_entities::CeWorkConfig::new(0, 0, u64::MAX),
    );
    let parent = TestParent::empty();
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

    let error = run_start_command(&mut provider, &scope, &parent, 2, scheduled).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::TreeUnavailable(_)
    ));
    assert_eq!(tree.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(provider.get_ordered_events(), ordered_before.as_slice());
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);

    run_start_command(&mut provider, &scope, &parent, 2, scheduled).unwrap();
    assert_eq!(tree.calls.load(Ordering::SeqCst), 2);
    assert_local_terminal_completed(&mut provider, wwd, day_limit);
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn test_ready_processing_missing_limit_fails_like_source() {
    with_storage(|storage| {
        let wwd_raw = 20260310u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
    });
}

#[test]
fn test_ready_processing_unknown_day_type_fails_and_returns_limit_to_promis() {
    with_storage(|storage| {
        let wwd_raw = 20260310u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
        let day_limit = U256::from(333u64);
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
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
            .unwrap();
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);

        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), day_limit);
    });
}

#[test]
fn test_ready_processing_zero_limit_fails() {
    with_storage(|storage| {
        let wwd_raw = 20260311u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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
        metadosis.set_metadosis_limit(wwd, U256::ZERO).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
    });
}

#[test]
fn test_ready_processing_no_tributes_returns_the_limit_to_promis() {
    with_storage(|storage| {
        let wwd_raw = 20260312u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
        let day_limit = U256::from(777u64);
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
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);

        // A red day is recorded as a supply-less brief; a day with no tributes issues nothing,
        // so its whole limit goes back to the warehouse.
        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&series).unwrap(),
            U256::ZERO
        );

        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), day_limit);
    });
}

// Stable historical ID retained after moving this invariant out of the
// four-node E2E lane.
// OCOMP-TEST-ID: OCM-E2E-002
#[test]
fn active_ocomp_profile_preserves_the_empty_day_compatibility_branch() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0313);
        let day_limit = U256::from(777);
        let scheduled = create_waiting_day(&storage, wwd, day_type::RED, day_limit);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_no_ocomp_job(&storage, wwd);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&series).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            day_limit
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            TributeContract::new(storage)
                .get_day_totals(wwd)
                .unwrap()
                .tribute_count,
            0
        );
    });
}

#[test]
fn green_empty_day_briefs_nothing_however_large_the_limit() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0321);
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::MAX);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), WwdStatus::Completed);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            outbe_desis::AuctionStage::from_u8(desis.auction_stage.read(&wwd).unwrap()).unwrap(),
            outbe_desis::AuctionStage::Briefed
        );
        assert_eq!(desis.brief_green.read(&wwd).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&wwd).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::MAX
        );
    });
}

#[test]
fn active_ocomp_profile_preserves_the_populated_zero_limit_branch() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0314);
        let nominal = U256::from(1_000);
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::ZERO);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        issue_one_tribute_and_run_metadosis(
            &storage,
            wwd,
            nominal,
            2,
            scheduled + SECONDS_PER_HOUR,
        );

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_no_ocomp_job(&storage, wwd);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        assert_eq!(desis.clearing_initiated.read(&series).unwrap(), 0);
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_amount, nominal);
    });
}

#[test]
fn active_ocomp_profile_preserves_the_populated_zero_lysis_limit_branch() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0318);
        let nominal = U256::from(1_000);
        // A red day divides supply by RED_DAY_REDUCTION_COEF. This non-zero
        // day limit therefore produces an exact zero Lysis allocation.
        let day_limit = U256::from(2);
        let scheduled = create_waiting_day(&storage, wwd, day_type::RED, day_limit);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        let tribute = TributeContract::new(storage.clone());
        tribute.total_supply.write(1).unwrap();
        let mut totals = outbe_tribute::schema::DayTotals::with_key(wwd);
        totals.initialized = true;
        totals.is_sealed = true;
        totals.tribute_count = 1;
        totals.tribute_nominal_amount = nominal;
        tribute.day_totals.create(&totals).unwrap();
        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_no_ocomp_job(&storage, wwd);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&series).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            day_limit
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_amount, nominal);
    });
}

#[test]
fn active_ocomp_profile_preserves_the_populated_unknown_day_branch() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0315);
        let nominal = U256::from(1_000);
        let day_limit = U256::from(333);
        let scheduled = create_waiting_day(&storage, wwd, day_type::UNKNOWN, day_limit);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        issue_one_tribute_and_run_metadosis(
            &storage,
            wwd,
            nominal,
            2,
            scheduled + SECONDS_PER_HOUR,
        );

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_no_ocomp_job(&storage, wwd);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        assert_eq!(desis.clearing_initiated.read(&series).unwrap(), 0);
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            day_limit
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_amount, nominal);
    });
}

#[test]
fn no_tributes_green_day_briefs_nothing_and_returns_the_limit() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(20260401u32);
        let day_limit = U256::from(10u64).pow(U256::from(26u64));
        let forming_start = wwd.start_timestamp();

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
        metadosis.set_wwd_day_type(wwd, WwdDayType::Green).unwrap();
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
            .unwrap();
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();

        let scheduled = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()
            .unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&series).unwrap(),
            U256::ZERO
        );

        let promis = PromisLimitContract::new(storage);
        assert_eq!(
            promis.get_total_unallocated().unwrap(),
            day_limit,
            "a day that earned nothing auctions nothing and leaves its limit on the warehouse"
        );
    });
}

#[test]
fn zero_limit_green_day_dispatches_no_brief() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(20260501u32);
        let forming_start = wwd.start_timestamp();

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
        metadosis.set_wwd_day_type(wwd, WwdDayType::Green).unwrap();
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
            .unwrap();

        let scheduled = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()
            .unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);

        let series = wwd;
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        assert_eq!(desis.clearing_initiated.read(&series).unwrap(), 0);
    });
}

#[test]
fn the_local_brief_prices_a_day_by_the_canonical_projection() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0805);
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::from(1_000_u64));
        arm_genesis_ocomp(&storage, CHAIN_ID);

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scheduled + SECONDS_PER_HOUR, CHAIN_ID),
            storage.clone(),
        );
        let mut metadosis = MetadosisContract::new(storage.clone());

        // A cold oracle prices nothing, and the empty table is load-bearing: it
        // is how Desis is told the day is unpriced, so it cancels the auction and
        // refunds the supply instead of opening one at a zero entry price.
        assert!(
            crate::settlement::day_entry_prices(&mut metadosis, &ctx, wwd)
                .unwrap()
                .is_empty()
        );

        // The COEN/USD pair is not registered here, so the rule the settlement
        // path used to carry of its own - which resolved every row through
        // `require_coen_pair` - could never price this day at all.
        assert!(outbe_oracle::api::require_coen_pair(storage.clone(), 840).is_err());

        // The day carries a WorldwideDay VWAP of its own and it is still not an
        // entry price: an auction is priced from the last closed UTC day alone.
        metadosis.set_wwd_vwap(wwd, U256::from(110_u64)).unwrap();
        let table = crate::settlement::day_entry_prices(&mut metadosis, &ctx, wwd).unwrap();
        assert!(table.is_empty());

        let projection =
            outbe_oracle::api::ocomp_pre_admission_projection(storage.clone(), ctx.block.timestamp)
                .unwrap();

        // One rule prices every day: the settlement table is the projection's,
        // less rows the oracle could not put a price on.
        assert_eq!(
            table
                .iter()
                .map(|row| (row.iso_code, row.entry_price_minor))
                .collect::<Vec<_>>(),
            projection
                .auction_entry_prices
                .iter()
                .filter(|row| !row.entry_price_minor.is_zero())
                .map(|row| (row.reference_currency, row.entry_price_minor))
                .collect::<Vec<_>>()
        );
    });
}
