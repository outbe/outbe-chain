use super::*;

fn seed_positive_ocomp_admission_fixture(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> (ExecutionScope, TestParent, u64) {
    let day_limit = U256::from(5_000u64) * U256::from(10u64).pow(U256::from(18u64));
    let nominal = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u64));
    let scheduled = StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        create_waiting_day(&storage, wwd, day_type::GREEN, day_limit)
    });
    let (scope, parent) = begin_persistent_active_scope(provider);
    StorageHandle::enter(provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        oracle.reference_currencies.push(840).unwrap();
        let previous_day = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(scheduled),
        );
        oracle
            .record_utc_day_vwap(previous_day, index, U256::from(250_000))
            .unwrap();
        oracle
            .exchange_rate
            .write(&index, U256::from(900_000))
            .unwrap();
        issue_one_tribute_in_scope(
            &storage,
            &scope,
            &parent,
            address!("7400000000000000000000000000000000000074"),
            wwd,
            nominal,
        );
    });
    (scope, parent, scheduled)
}

#[test]
fn positive_ocomp_admission_rolls_back_every_mutation_and_retries_exactly() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0819);
    let block_number = 2;

    let mut control = HashMapStorageProvider::new(CHAIN_ID);
    let (control_scope, control_parent, scheduled) =
        seed_positive_ocomp_admission_fixture(&mut control, wwd);
    control.fail_after_mutation_at(usize::MAX);
    run_start_command(
        &mut control,
        &control_scope,
        &control_parent,
        block_number,
        scheduled,
    )
    .unwrap();
    let mutation_count = control.clear_mutation_failure();
    StorageHandle::enter(&mut control, |storage| {
        assert_eq!(
            outbe_nod::api::entry_price_snapshot(storage, wwd).unwrap(),
            Some(std::collections::BTreeMap::from([(
                840,
                U256::from(250_000)
            )]))
        );
    });
    assert!(
        mutation_count >= 6,
        "positive OCOMP admission must persist pre-admission, scheduler, outer state and events"
    );
    let clean_storage = control.storage.clone();
    let clean_events = control.events.clone();
    let clean_ordered_events = control.get_ordered_events().to_vec();
    let clean_ce_work = control_scope.ce_work_checkpoint().unwrap();
    end_persistent_active_scope(&mut control, &control_scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let (scope, parent, scheduled) = seed_positive_ocomp_admission_fixture(&mut provider, wwd);
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_events_before = provider.get_ordered_events().to_vec();
        let ce_before = scope.ce_work_checkpoint().unwrap();
        provider.fail_after_mutation_at(operation);

        assert!(matches!(
            run_start_command(&mut provider, &scope, &parent, block_number, scheduled,),
            Err(outbe_primitives::error::PrecompileError::Storage(_))
        ));
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            ordered_events_before.as_slice(),
            "ordered events at {operation}"
        );
        assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);

        run_start_command(&mut provider, &scope, &parent, block_number, scheduled).unwrap();
        assert_eq!(
            provider.storage, clean_storage,
            "storage retry at {operation}"
        );
        assert_eq!(provider.events, clean_events, "events retry at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            clean_ordered_events.as_slice(),
            "ordered events retry at {operation}"
        );
        assert_eq!(scope.ce_work_checkpoint().unwrap(), clean_ce_work);
        end_persistent_active_scope(&mut provider, &scope);
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
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();
    let ce_before = scope.ce_work_checkpoint().unwrap();

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    let error = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, offering_entry, CHAIN_ID),
            storage,
        );
        crate::commands::advance_active_worldwide_days(&ctx, &scope)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Fatal(_)
    ));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            MetadosisContract::new(storage).get_wwd_status(wwd).unwrap(),
            status::FORMING
        );
    });
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn absent_profile_rejects_populated_ready_before_failed_state_or_lysis_effects() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0804);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let scheduled = StorageHandle::enter(&mut provider, |storage| {
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::from(1_000));
        MetadosisContract::new(storage)
            .fixture_set_wwd_status(wwd, WwdStatus::Ready)
            .unwrap();
        scheduled
    });
    let (scope, parent) = begin_persistent_active_scope(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        issue_one_tribute_in_scope(
            &storage,
            &scope,
            &parent,
            address!("7700000000000000000000000000000000000077"),
            wwd,
            U256::from(10),
        );
    });
    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();
    let ce_before = scope.ce_work_checkpoint().unwrap();

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    let error = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, scheduled, CHAIN_ID),
            storage,
        );
        crate::commands::start_metadosis(&ctx, &scope, &parent)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Fatal(_)
    ));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::READY);
        assert_no_ocomp_job(&storage, wwd);
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(TributeContract::new(storage).total_supply().unwrap(), 1);
    });
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn active_ocomp_profile_discovers_later_ready_day_after_first_was_indexed() {
    with_storage(|storage| {
        let first_wwd = outbe_primitives::time::WorldwideDay::new(2026_0316);
        let second_wwd = outbe_primitives::time::WorldwideDay::new(2026_0317);
        let nominal = U256::from(1_000);
        let first_scheduled =
            create_waiting_day(&storage, first_wwd, day_type::GREEN, U256::from(800));
        let second_scheduled =
            create_waiting_day(&storage, second_wwd, day_type::GREEN, U256::from(900));
        let timestamp = first_scheduled.max(second_scheduled) + SECONDS_PER_HOUR;
        arm_genesis_ocomp(&storage, CHAIN_ID);

        with_active_scope(storage.clone(), |scope, parent| {
            issue_one_tribute_in_scope(
                &storage,
                scope,
                parent,
                address!("7400000000000000000000000000000000000074"),
                first_wwd,
                nominal,
            );
            issue_one_tribute_in_scope(
                &storage,
                scope,
                parent,
                address!("7500000000000000000000000000000000000075"),
                second_wwd,
                nominal,
            );

            let first_ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(10, timestamp, CHAIN_ID),
                storage.clone(),
            );
            crate::commands::start_metadosis(&first_ctx, scope, parent).unwrap();

            let after_first = MetadosisContract::new(storage.clone());
            assert!(!after_first
                .ocomp_fsm_states
                .get_bytes(&first_wwd)
                .is_empty()
                .unwrap());
            assert!(after_first
                .ocomp_fsm_states
                .get_bytes(&second_wwd)
                .is_empty()
                .unwrap());

            let second_ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(11, timestamp + 1, CHAIN_ID),
                storage.clone(),
            );
            crate::commands::start_metadosis(&second_ctx, scope, parent).unwrap();
        });

        let metadosis = MetadosisContract::new(storage);
        let schema_limits = crate::ocomp::schema::poc_schema_limits();
        let first = metadosis
            .ocomp_fsm_state(first_wwd, &schema_limits)
            .unwrap()
            .projection();
        let second = metadosis
            .ocomp_fsm_state(second_wwd, &schema_limits)
            .unwrap()
            .projection();
        assert_eq!(first.phase, crate::ocomp::state::DayPhase::Ready);
        assert_eq!(first.next_check_height, Some(10));
        assert_eq!(second.phase, crate::ocomp::state::DayPhase::Ready);
        assert_eq!(second.next_check_height, Some(11));
        assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
    });
}

#[test]
fn populated_positive_gratis_day_enqueues_ocomp_without_synchronous_lysis() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(2026_0313);
        let day_limit = U256::from(5_000u64) * U256::from(10u64).pow(U256::from(18u64));
        let nominal = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u64));
        let owner = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, day_limit);
        arm_genesis_ocomp(&storage, CHAIN_ID);

        with_active_scope(storage.clone(), |scope, parent| {
            issue_one_tribute_in_scope(&storage, scope, parent, owner, wwd, nominal);

            // This collides with the NOD the removed synchronous Lysis path
            // would have attempted to issue. OCOMP admission must not touch it.
            outbe_nodfactory::api::issue_nod(
                &storage,
                scope,
                parent,
                &outbe_nod::NodIssueParams {
                    owner,
                    gratis_load_minor: U256::from(1),
                    worldwide_day: wwd,
                    league_id: 1,
                    floor_price_minor: U256::from(1),
                    entry_price_minor: U256::from(1),
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            )
            .unwrap();

            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(
                    2,
                    scheduled + SECONDS_PER_HOUR,
                    outbe_primitives::chain::CHAIN_ID,
                ),
                storage.clone(),
            );
            crate::commands::start_metadosis(&ctx, scope, parent).unwrap();
        });

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::READY);
        assert!(metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(!metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        let limits = crate::ocomp::schema::poc_schema_limits();
        let _profile = metadosis
            .read_ocomp_request_profile(&limits)
            .unwrap()
            .unwrap();
        let fsm = metadosis
            .ocomp_fsm_state(wwd, &limits)
            .unwrap()
            .projection();
        assert_eq!(fsm.phase, crate::ocomp::state::DayPhase::Ready);
        assert_eq!(fsm.next_check_height, Some(2));
        assert!(
            metadosis
                .ocomp_pre_admission_projection(wwd)
                .unwrap()
                .initialized
        );
        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.total_supply().unwrap(), 1);
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 1);
        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::ZERO);
    });
}
