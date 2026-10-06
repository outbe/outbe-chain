use super::fixtures::begin_scope_with_persisted_parent;
use super::*;
use crate::tests::capacity::begin_fixed_partition_scope;
use outbe_compressed_entities::RetirementOutcome;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyOutcome {
    Completed,
    CompletedForfeited,
    FailedForfeited,
}

#[derive(Clone, Copy)]
struct LocalDay {
    wwd: outbe_primitives::time::WorldwideDay,
    day_type: u8,
    day_limit: U256,
    tribute_count: u32,
    tribute_nominal: U256,
}

fn seed_local_terminal_fixture(provider: &mut HashMapStorageProvider, day: LocalDay) -> u64 {
    let LocalDay {
        wwd,
        day_type,
        day_limit,
        tribute_count,
        tribute_nominal,
    } = day;
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
                tribute_nominal_total_minor: tribute_nominal,
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
        assert_eq!(admission.tribute_nominal_total_minor, tribute_nominal);
        scheduled
    })
}

fn seal_fresh_tribute_day(storage: &StorageHandle, wwd: outbe_primitives::time::WorldwideDay) {
    let mut tribute = TributeContract::new(storage.clone());
    tribute.initialize_fresh_ocomp_profile().unwrap();
    tribute.seal_day(wwd).unwrap();
}

fn begin_ready_scope(
    provider: &mut HashMapStorageProvider,
    outcome: ReadyOutcome,
) -> (ExecutionScope, TestParent) {
    match outcome {
        ReadyOutcome::Completed => begin_persistent_active_scope(provider),
        ReadyOutcome::CompletedForfeited | ReadyOutcome::FailedForfeited => {
            (begin_fixed_partition_scope(provider).0, TestParent::empty())
        }
    }
}

fn end_ready_scope(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    outcome: ReadyOutcome,
) {
    // The fixed partition tree cannot seal a block.
    if outcome == ReadyOutcome::Completed {
        end_persistent_active_scope(provider, scope);
    }
}

fn assert_ready_outcome(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    outcome: ReadyOutcome,
    expected_promis: U256,
) {
    match outcome {
        ReadyOutcome::Completed => assert_local_terminal_completed(provider, wwd, expected_promis),
        ReadyOutcome::CompletedForfeited => {
            assert_local_terminal_completed(provider, wwd, expected_promis);
            assert_tribute_partition_forfeited(provider, wwd);
        }
        ReadyOutcome::FailedForfeited => assert_ready_day_forfeited(provider, wwd, expected_promis),
    }
}

fn assert_ready_day_forfeited(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    day_limit: U256,
) {
    assert_local_terminal_outcome(provider, wwd, status::FAILED, day_limit);
    StorageHandle::enter(provider, |storage| {
        let receipt = MetadosisContract::new(storage.clone())
            .read_metadosis_failure_receipt(wwd, day_limit)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, day_limit);
        assert_eq!(receipt.carry_over_after, day_limit);
        assert_eq!(receipt.retirement, RetirementOutcome::Requested);
        assert_no_ocomp_job(&storage, wwd);
    });
    assert_tribute_partition_forfeited(provider, wwd);
}

fn assert_tribute_partition_forfeited(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) {
    StorageHandle::enter(provider, |storage| {
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 0);
        let totals = tribute.get_day_totals(wwd).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_total_minor, U256::ZERO);
        assert_eq!(
            tribute
                .pre_admission_projection(wwd)
                .unwrap()
                .source_generation,
            1
        );
    });
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
    day: LocalDay,
    outcome: ReadyOutcome,
    expected_promis: U256,
) {
    let LocalDay {
        wwd,
        day_type,
        day_limit,
        tribute_count,
        tribute_nominal,
    } = day;
    let block_number = 2;

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut probe,
        LocalDay {
            wwd,
            day_type,
            day_limit,
            tribute_count,
            tribute_nominal,
        },
    );
    let (probe_scope, probe_parent) = begin_ready_scope(&mut probe, outcome);
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
    if outcome == ReadyOutcome::Completed {
        assert_eq!(probe_scope.ce_work_checkpoint().unwrap(), ce_before_probe);
    }
    assert_ready_outcome(&mut probe, wwd, outcome, expected_promis);
    let clean_storage = probe.storage.clone();
    let clean_events = probe.events.clone();
    let clean_ordered_events = probe.get_ordered_events().to_vec();
    let clean_ce_work = probe_scope.ce_work_checkpoint().unwrap();
    end_ready_scope(&mut probe, &probe_scope, outcome);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let scheduled = seed_local_terminal_fixture(
            &mut provider,
            LocalDay {
                wwd,
                day_type,
                day_limit,
                tribute_count,
                tribute_nominal,
            },
        );
        let (scope, parent) = begin_ready_scope(&mut provider, outcome);
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
        assert_ready_outcome(&mut provider, wwd, outcome, expected_promis);
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
        end_ready_scope(&mut provider, &scope, outcome);
    }
}

#[test]
fn empty_tribute_day_command_rolls_back_every_mutation_and_ce_work_then_retries() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0812),
            day_type: day_type::RED,
            day_limit: U256::from(777),
            tribute_count: 0,
            tribute_nominal: U256::ZERO,
        },
        ReadyOutcome::Completed,
        U256::from(777),
    );
}

#[test]
fn zero_gratis_command_rolls_back_every_mutation_and_ce_work_then_retries() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0813),
            day_type: day_type::RED,
            day_limit: U256::from(2),
            tribute_count: 1,
            tribute_nominal: U256::from(1_000),
        },
        ReadyOutcome::CompletedForfeited,
        U256::from(2),
    );
}

#[test]
fn zero_day_limit_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0820),
            day_type: day_type::GREEN,
            day_limit: U256::ZERO,
            tribute_count: 1,
            tribute_nominal: U256::from(1_000),
        },
        ReadyOutcome::FailedForfeited,
        U256::ZERO,
    );
}

#[test]
fn unknown_day_type_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0821),
            day_type: day_type::UNKNOWN,
            day_limit: U256::from(777),
            tribute_count: 1,
            tribute_nominal: U256::from(1_000),
        },
        ReadyOutcome::FailedForfeited,
        U256::from(777),
    );
}

#[test]
fn green_empty_tribute_day_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0822),
            day_type: day_type::GREEN,
            day_limit: U256::from(777),
            tribute_count: 0,
            tribute_nominal: U256::ZERO,
        },
        ReadyOutcome::Completed,
        U256::from(777),
    );
}

#[test]
fn green_zero_gratis_rolls_back_every_mutation_and_retries_exactly() {
    exercise_local_terminal_fault_matrix(
        LocalDay {
            wwd: outbe_primitives::time::WorldwideDay::new(2026_0823),
            day_type: day_type::GREEN,
            day_limit: U256::from(2),
            tribute_count: 1,
            tribute_nominal: U256::ONE,
        },
        ReadyOutcome::CompletedForfeited,
        U256::ONE,
    );
}

#[test]
fn zero_gratis_completes_and_forfeits_its_present_parent_partition() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0818);
    let day_limit = U256::from(2);
    let nominal = U256::from(1_000);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut provider,
        LocalDay {
            wwd,
            day_type: day_type::RED,
            day_limit,
            tribute_count: 1,
            tribute_nominal: nominal,
        },
    );
    let (scope, tree) = begin_fixed_partition_scope(&mut provider);

    run_start_command(&mut provider, &scope, &TestParent::empty(), 2, scheduled).unwrap();

    assert_local_terminal_completed(&mut provider, wwd, day_limit);
    assert_tribute_partition_forfeited(&mut provider, wwd);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            TributeContract::new(storage.clone())
                .pre_admission_projection(wwd)
                .unwrap()
                .sealed_collection_root,
            tree.partition_root
        );
        assert!(MetadosisContract::new(storage.clone())
            .read_metadosis_failure_receipt(wwd, day_limit)
            .unwrap()
            .is_none());
        assert_no_ocomp_job(&storage, wwd);
    });
    let retired = provider
        .get_ordered_events()
        .iter()
        .filter(|event| {
            outbe_tribute::precompile::ITribute::TributePartitionRetired::decode_log(event).is_ok()
        })
        .count();
    assert_eq!(retired, 1);
}

#[test]
fn empty_tribute_day_restores_outer_ce_checkpoint_after_late_parent_failure_then_retries() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0814);
    let day_limit = U256::from(777);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut provider,
        LocalDay {
            wwd,
            day_type: day_type::RED,
            day_limit,
            tribute_count: 0,
            tribute_nominal: U256::ZERO,
        },
    );
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
    begin_scope_with_persisted_parent(&mut provider, &scope, parent_root);
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
        seal_fresh_tribute_day(&storage, wwd);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(metadosis
            .read_metadosis_failure_receipt(wwd, U256::ZERO)
            .unwrap()
            .is_some());
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
        seal_fresh_tribute_day(&storage, wwd);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(metadosis
            .read_metadosis_failure_receipt(wwd, day_limit)
            .unwrap()
            .is_some());

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
        seal_fresh_tribute_day(&storage, wwd);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(metadosis
            .read_metadosis_failure_receipt(wwd, U256::ZERO)
            .unwrap()
            .is_some());
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

        // A red day is recorded as a brief with no limit. A day with no tributes issues nothing,
        // so its whole limit returns to the warehouse.
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

fn assert_populated_ready_day_fails_and_forfeits(
    wwd: outbe_primitives::time::WorldwideDay,
    dtype: u8,
    day_limit: U256,
) {
    let nominal = U256::from(1_000);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut provider,
        LocalDay {
            wwd,
            day_type: dtype,
            day_limit,
            tribute_count: 1,
            tribute_nominal: nominal,
        },
    );
    let (scope, tree) = begin_fixed_partition_scope(&mut provider);

    run_start_command(&mut provider, &scope, &TestParent::empty(), 2, scheduled).unwrap();

    assert_ready_day_forfeited(&mut provider, wwd, day_limit);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            TributeContract::new(storage.clone())
                .pre_admission_projection(wwd)
                .unwrap()
                .sealed_collection_root,
            tree.partition_root
        );
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&wwd).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        assert_eq!(desis.clearing_initiated.read(&wwd).unwrap(), 0);
        assert_eq!(NodContract::new(storage).total_supply().unwrap(), 0);
    });
    let executed = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisExecuted::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].status, "FAILED");
    assert_eq!(executed[0].tributeNominalTotalMinor, nominal);
    assert_eq!(executed[0].promisLimitReturnedMinor, day_limit);
}

#[test]
fn active_ocomp_profile_fails_the_populated_zero_limit_day_and_forfeits_its_partition() {
    assert_populated_ready_day_fails_and_forfeits(
        outbe_primitives::time::WorldwideDay::new(2026_0314),
        day_type::GREEN,
        U256::ZERO,
    );
}

#[test]
fn active_ocomp_profile_completes_the_populated_zero_lysis_limit_day_and_forfeits_its_partition() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0318);
    let nominal = U256::from(1_000);
    // A red day divides the day gratis limit by RED_DAY_REDUCTION_COEF. This
    // non-zero day limit therefore produces an exact zero Lysis Limit.
    let day_limit = U256::from(2);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = seed_local_terminal_fixture(
        &mut provider,
        LocalDay {
            wwd,
            day_type: day_type::RED,
            day_limit,
            tribute_count: 1,
            tribute_nominal: nominal,
        },
    );
    let (scope, _) = begin_fixed_partition_scope(&mut provider);

    run_start_command(&mut provider, &scope, &TestParent::empty(), 2, scheduled).unwrap();

    assert_local_terminal_completed(&mut provider, wwd, day_limit);
    assert_tribute_partition_forfeited(&mut provider, wwd);
    StorageHandle::enter(&mut provider, |storage| {
        assert_no_ocomp_job(&storage, wwd);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&wwd).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&wwd).unwrap(), 0);
        assert_eq!(
            desis.pending_desis_limit_minor.read(&wwd).unwrap(),
            U256::ZERO
        );
        assert_eq!(NodContract::new(storage).total_supply().unwrap(), 0);
    });
}

#[test]
fn active_ocomp_profile_fails_the_populated_unknown_day_and_forfeits_its_partition() {
    assert_populated_ready_day_fails_and_forfeits(
        outbe_primitives::time::WorldwideDay::new(2026_0315),
        day_type::UNKNOWN,
        U256::from(333),
    );
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
        seal_fresh_tribute_day(&storage, wwd);

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

struct PersistentTree {
    directory: std::path::PathBuf,
    service: outbe_compressed_entities::CompressedTreeService,
}

impl PersistentTree {
    fn open(name: &str) -> Self {
        use outbe_compressed_entities::{
            CandidateCacheLimits, CeMdbx, CeTopologyV1, CompressedTreeService, EnvironmentIdentity,
            FinalizedMarker, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
        };
        let directory =
            std::env::temp_dir().join(format!("outbe-metadosis-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let db = CeMdbx::open(
            &directory,
            EnvironmentIdentity {
                local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                chain_id: CHAIN_ID,
                genesis_hash: B256::ZERO,
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                topology: CeTopologyV1.encode(),
                tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: B256::ZERO,
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
            },
        )
        .unwrap();
        let service = CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap();
        Self { directory, service }
    }

    fn scope_at(&self, block_number: u64, block_hash: B256, root: B256) -> ExecutionScope {
        let parent = self
            .service
            .open_parent(outbe_compressed_entities::ExactParentIdentity {
                commitment_scheme_version: outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME,
                block_number,
                block_hash,
                root,
            })
            .unwrap();
        ExecutionScope::with_parent_tree(
            parent,
            outbe_compressed_entities::CeWorkConfig::new(0, 0, u64::MAX),
        )
    }

    fn finalize(&self, block_number: u64, output: outbe_compressed_entities::SealOutput) -> B256 {
        let block_hash = B256::repeat_byte(u8::try_from(block_number).unwrap());
        self.service
            .publish_candidate(block_hash, output.staged_tree_batch)
            .unwrap();
        self.service
            .apply_finalized(block_number, block_hash, output.new_root)
            .unwrap();
        block_hash
    }
}

impl Drop for PersistentTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Mints one real Tribute in a sealed block, then settles the READY day in the
/// next block through the end-of-block seal.
fn settle_minted_ready_day_through_the_seal(
    name: &str,
    wwd: outbe_primitives::time::WorldwideDay,
    dtype: u8,
    day_limit: U256,
    nominal: U256,
) -> HashMapStorageProvider {
    let tree = PersistentTree::open(name);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let parent = TestParent::empty();
    let genesis_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let mint_scope = tree.scope_at(0, B256::ZERO, genesis_root);
    provider.set_block_number(1);
    let (scheduled, minted) = StorageHandle::enter(&mut provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        let scheduled = create_waiting_day(&storage, wwd, dtype, day_limit);
        super::arm_reference_price(&storage, scheduled);
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(genesis_root.as_slice()),
            )
            .unwrap();
        begin_block(storage.clone(), &mint_scope).unwrap();
        issue_one_tribute_in_scope(
            &storage,
            &mint_scope,
            &parent,
            FixtureTribute {
                owner: address!("7400000000000000000000000000000000000074"),
                wwd,
                nominal,
            },
        );
        (scheduled, end_block(storage, &mint_scope).unwrap())
    });
    let minted_root = minted.new_root;
    let minted_hash = tree.finalize(1, minted);

    let settle_scope = tree.scope_at(1, minted_hash, minted_root);
    provider.set_block_number(2);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage, &settle_scope).unwrap();
    });
    assert!(settle_scope
        .authenticated_partition_root(outbe_compressed_entities::PartitionRef::TributeWwd(wwd))
        .unwrap()
        .is_some());
    run_start_command(&mut provider, &settle_scope, &parent, 2, scheduled).unwrap();
    let settled = StorageHandle::enter(&mut provider, |storage| {
        end_block(storage, &settle_scope).unwrap()
    });
    let settled_root = settled.new_root;
    let settled_hash = tree.finalize(2, settled);

    let retired = tree
        .service
        .open_parent(outbe_compressed_entities::ExactParentIdentity {
            commitment_scheme_version: outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME,
            block_number: 2,
            block_hash: settled_hash,
            root: settled_root,
        })
        .unwrap();
    assert!(!retired
        .partition_present_verified(
            outbe_compressed_entities::PartitionRef::TributeWwd(wwd),
            settled_root
        )
        .unwrap());
    assert_tribute_partition_forfeited(&mut provider, wwd);
    provider
}

#[test]
fn minted_zero_limit_ready_day_fails_and_retires_its_partition_through_the_seal() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0316);
    let mut provider = settle_minted_ready_day_through_the_seal(
        "zero-limit",
        wwd,
        day_type::GREEN,
        U256::ZERO,
        U256::from(1_000),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        let receipt = metadosis
            .read_metadosis_failure_receipt(wwd, U256::ZERO)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.retirement, RetirementOutcome::Requested);
    });
}

#[test]
fn minted_zero_gratis_day_completes_and_retires_its_partition_through_the_seal() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0317);
    let mut provider = settle_minted_ready_day_through_the_seal(
        "zero-gratis",
        wwd,
        day_type::RED,
        U256::from(2),
        U256::from(1_000),
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            MetadosisContract::new(storage).get_wwd_status(wwd).unwrap(),
            status::COMPLETED
        );
    });
}
