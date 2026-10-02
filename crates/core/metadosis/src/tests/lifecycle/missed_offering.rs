use super::*;

fn run_missed_offering_command(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    run_advance_command(provider, scope, block_number, timestamp)
}

fn seed_unformed_missed_offering_day(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    carry_over: U256,
) -> u64 {
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(carry_over)
            .unwrap();
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
        TributeContract::new(storage.clone()).seal_day(wwd).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .offering_end()
            .read()
            .unwrap()
    })
}

#[test]
fn missed_offering_routes_the_formed_limit_once_and_exposes_a_durable_receipt() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0731);
    let base_limit = U256::from(100);
    let formation_carry = U256::from(9);
    let later_carry = U256::from(7);
    // Formation no longer folds the accumulator in, so the day is formed against its own base.
    let formed_limit = base_limit;
    let carried = formation_carry + later_carry;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, base_limit, formation_carry, later_carry);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);

    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        let receipt = metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .unwrap();
        assert!(metadosis
            .read_capacity_forfeiture_receipt(wwd)
            .unwrap()
            .is_none());
        assert_eq!(receipt.value_routed, formed_limit);
        assert_eq!(receipt.carry_over_before, carried);
        assert_eq!(receipt.carry_over_after, carried + formed_limit);
        assert_eq!(receipt.block_number, 2);
        assert_eq!(
            receipt.retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            carried + formed_limit
        );
        assert_no_ocomp_job(&storage, wwd);
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            TributeContract::new(storage.clone())
                .get_day_totals(wwd)
                .unwrap()
                .tribute_count,
            0
        );
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&wwd).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );

        let call = IMetadosis::getWorldwideDayTerminalReceiptCall { wwd: wwd.value() };
        let output =
            metadosis_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
        let decoded =
            IMetadosis::getWorldwideDayTerminalReceiptCall::abi_decode_returns(&output).unwrap();
        assert_eq!(decoded.outcome, 1);
        assert_eq!(decoded.valueRouted, formed_limit);
        assert_eq!(decoded.carryOverBefore, carried);
        assert_eq!(decoded.carryOverAfter, carried + formed_limit);
        assert_eq!(
            decoded.retirementOutcome,
            crate::schema::terminal_retirement::NOT_PRESENT
        );
        assert_eq!(decoded.blockNumber, 2);
    });

    let missed_events_before = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::WorldwideDayMissedOffering::decode_log(event).ok())
        .count();
    assert_eq!(missed_events_before, 1);
    let storage_before_replay = provider.storage.clone();
    let events_before_replay = provider.events.clone();
    run_missed_offering_command(&mut provider, &scope, 3, offering_end + 1).unwrap();
    assert_eq!(provider.storage, storage_before_replay);
    assert_eq!(provider.events, events_before_replay);

    end_persistent_active_scope(&mut provider, &scope);
}

fn seed_zero_limit_lookback_day_opening_at(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    opening: u64,
) {
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                wwd,
                opening - FORMING_PERIOD_HOURS * SECONDS_PER_HOUR,
                0,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::LookbackDelay)
            .unwrap();
        TributeContract::new(storage).seal_day(wwd).unwrap();
    });
}

fn assert_zero_limit_offering_missed(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    block_number: u64,
) {
    StorageHandle::enter(provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        let receipt = metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, U256::ZERO);
        assert_eq!(receipt.block_number, block_number);
        assert!(TributeContract::new(storage).is_day_sealed(wwd).unwrap());
    });
    assert!(provider_status_events(provider, wwd)
        .iter()
        .all(|(_, new_status, _)| *new_status != status::OFFERING));
}

#[test]
fn final_zero_limit_misses_the_offering_from_forming_and_lookback() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0901);
    for from_lookback in [false, true] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_unformed_missed_offering_day(&mut provider, wwd, U256::ZERO);
        let (forming_end, lookback_end) = StorageHandle::enter(&mut provider, |storage| {
            let day = MetadosisContract::new(storage).worldwide_days.entry(wwd);
            (
                day.forming_end().read().unwrap(),
                day.lookback_end().read().unwrap(),
            )
        });
        let (scope, _parent) = begin_persistent_active_scope(&mut provider);
        let mut block_number = 2;
        if from_lookback {
            run_advance_command(&mut provider, &scope, block_number, forming_end).unwrap();
            block_number += 1;
        }

        run_advance_command(&mut provider, &scope, block_number, lookback_end).unwrap();

        assert_zero_limit_offering_missed(&mut provider, wwd, block_number);
        end_persistent_active_scope(&mut provider, &scope);
    }
}

#[test]
fn zero_limit_offering_opens_until_its_utc_day_closes() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0902);
    let utc_day_closed = wwd.to_timestamp_utc() + 24 * SECONDS_PER_HOUR;

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let last_open_hour = utc_day_closed - SECONDS_PER_HOUR;
    seed_zero_limit_lookback_day_opening_at(&mut provider, wwd, last_open_hour);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_advance_command(&mut provider, &scope, 2, last_open_hour).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);
        assert!(metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .is_none());
        assert!(!TributeContract::new(storage).is_day_sealed(wwd).unwrap());
    });
    end_persistent_active_scope(&mut provider, &scope);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_zero_limit_lookback_day_opening_at(&mut provider, wwd, utc_day_closed);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_advance_command(&mut provider, &scope, 2, utc_day_closed).unwrap();
    assert_zero_limit_offering_missed(&mut provider, wwd, 2);
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn final_zero_limit_miss_from_forming_keeps_the_rate_for_the_next_day() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0302);
    let next = outbe_primitives::time::WorldwideDay::new(2026_0303);
    let forming = |day: outbe_primitives::time::WorldwideDay| {
        (
            day.start_timestamp(),
            day.start_timestamp() + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR,
        )
    };
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_unformed_missed_offering_day(&mut provider, wwd, U256::ZERO);
    let lookback_end = StorageHandle::enter(&mut provider, |storage| {
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        let previous = wwd.previous_date_key();
        for (at, price) in [
            (forming(previous).0 + SECONDS_PER_HOUR, 100u64),
            (forming(wwd).0 + 30 * SECONDS_PER_HOUR, 110),
            (forming(next).0 + 30 * SECONDS_PER_HOUR, 130),
        ] {
            oracle
                .write_snapshot(at, &[(pair, U256::from(price), U256::ONE)])
                .unwrap();
        }
        let (start, end) = forming(previous);
        oracle
            .store_worldwide_day_vwap_snapshot(previous, start, end)
            .unwrap();
        MetadosisContract::new(storage)
            .worldwide_days
            .entry(wwd)
            .lookback_end()
            .read()
            .unwrap()
    });
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);

    run_advance_command(&mut provider, &scope, 2, lookback_end).unwrap();

    assert_zero_limit_offering_missed(&mut provider, wwd, 2);
    assert_eq!(
        provider_status_events(&provider, wwd),
        vec![
            (status::FORMING, status::LOOKBACK_DELAY, 2),
            (status::LOOKBACK_DELAY, status::FAILED, 2),
        ]
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_oracle::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
            Some(U256::from(110u64))
        );
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::GREEN);
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                next,
                next.start_timestamp(),
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(next).unwrap();
        metadosis.set_metadosis_limit(next, U256::ONE).unwrap();
        TributeContract::new(storage).seal_day(next).unwrap();
    });

    run_advance_command(&mut provider, &scope, 3, lookback_end + SECONDS_PER_HOUR).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let day = MetadosisContract::new(storage).worldwide_days.entry(next);
        assert_eq!(day.previous_vwap().read().unwrap(), U256::from(110u64));
        assert_eq!(day.day_type().read().unwrap(), day_type::GREEN);
    });
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn limit_settled_earlier_in_the_opening_tick_opens_the_offering() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0903);
    let settled_limit = U256::from(777);
    let tick = wwd.to_timestamp_utc() + 24 * SECONDS_PER_HOUR + 60;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_zero_limit_lookback_day_opening_at(&mut provider, wwd, tick);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(&mut provider, |storage| {
        // Cycle settles UTC day N at the first tick of N + 1, stamped at N's midnight.
        let settlement = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, wwd.to_timestamp_utc(), CHAIN_ID),
            storage,
        );
        crate::commands::apply_cycle_day_limit(&settlement, settled_limit).unwrap();
    });
    run_advance_command(&mut provider, &scope, 2, tick).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            settled_limit
        );
        assert!(!TributeContract::new(storage).is_day_sealed(wwd).unwrap());
    });
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn malformed_missed_offering_receipt_is_fatal() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0801);
    with_contract(|metadosis| {
        metadosis
            .write_missed_offering_receipt(crate::terminal::MissedOfferingReceipt {
                worldwide_day: wwd,
                value_routed: U256::from(10),
                carry_over_before: U256::from(3),
                carry_over_after: U256::from(12),
                retirement: outbe_compressed_entities::RetirementOutcome::NotPresent,
                block_number: 1,
            })
            .unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn missed_receipt_value_drift_is_fatal_in_reader_and_aggregate() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0802);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, U256::from(100), U256::ZERO, U256::ZERO);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        let mut receipt = metadosis.read_terminal_receipt(wwd).unwrap().unwrap();
        receipt.common_mut().value_routed += U256::from(1);
        receipt.common_mut().carry_over_after += U256::from(1);
        metadosis
            .worldwide_day_terminal_receipts
            .get_bytes(&wwd)
            .write(&crate::terminal::codec::encode(&receipt))
            .unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn missed_receipt_with_trailing_capacity_payload_is_fatal_in_reader_and_aggregate() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0803);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, U256::from(100), U256::ZERO, U256::ZERO);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        let bytes = metadosis.worldwide_day_terminal_receipts.get_bytes(&wwd);
        let mut malformed = bytes.read().unwrap();
        malformed.extend_from_slice(&[0; 92]);
        bytes.write(&malformed).unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn missed_receipt_status_drift_is_fatal_in_reader_and_aggregate() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0804);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, U256::from(100), U256::ZERO, U256::ZERO);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .fixture_set_wwd_status(wwd, WwdStatus::Completed)
            .unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn missed_receipt_membership_drift_is_fatal_in_reader_and_aggregate() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0805);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, U256::from(100), U256::ZERO, U256::ZERO);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis.add_active_wwd(wwd).unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn duplicate_closed_membership_is_fatal_in_reader_and_aggregate() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0806);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, U256::from(100), U256::ZERO, U256::ZERO);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);
    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        metadosis.closed_wwd.push_back(wwd).unwrap();

        assert!(matches!(
            metadosis.read_missed_offering_receipt(wwd),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn missed_offering_rejects_a_populated_partition_without_any_partial_effect() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0801);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end = seed_missed_offering_day(
        &mut provider,
        wwd,
        U256::from(100),
        U256::ZERO,
        U256::from(7),
    );
    let (scope, parent) = begin_persistent_active_scope(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        issue_one_tribute_in_scope(
            &storage,
            &scope,
            &parent,
            address!("7600000000000000000000000000000000000076"),
            wwd,
            U256::from(10),
        );
    });

    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();
    let ce_before = scope.ce_work_checkpoint().unwrap();
    let error = run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::BodyReadCorruption(_)
    ));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FORMING);
        assert!(metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .is_none());
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::from(7)
        );
    });

    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn missed_offering_rolls_back_every_injected_storage_or_event_failure_then_retries() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0802);
    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut probe, wwd, U256::from(100), U256::ZERO, U256::from(7));
    let (probe_scope, _parent) = begin_persistent_active_scope(&mut probe);
    probe.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    probe.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut probe, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, offering_end, CHAIN_ID),
            storage,
        );
        crate::commands::advance_active_worldwide_days(&ctx, &probe_scope).unwrap();
    });
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 6,
        "MissedOffering must touch Promis, retirement, receipt, WWD indexes and events"
    );
    let clean_storage = probe.storage.clone();
    let clean_events = probe.events.clone();
    let clean_ordered_events = probe.get_ordered_events().to_vec();
    let clean_ce_work = probe_scope.ce_work_checkpoint().unwrap();
    end_persistent_active_scope(&mut probe, &probe_scope);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let offering_end = seed_missed_offering_day(
            &mut provider,
            wwd,
            U256::from(100),
            U256::ZERO,
            U256::from(7),
        );
        let (scope, _parent) = begin_persistent_active_scope(&mut provider);
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_events_before = provider.get_ordered_events().to_vec();
        let ce_before = scope.ce_work_checkpoint().unwrap();

        provider.enable_metadosis_mutation_frame(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        );
        provider.fail_after_mutation_at(operation);
        let result = StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(2, offering_end, CHAIN_ID),
                storage,
            );
            crate::commands::advance_active_worldwide_days(&ctx, &scope)
        });
        assert!(
            result.is_err(),
            "mutation {operation} unexpectedly succeeded"
        );
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            ordered_events_before.as_slice(),
            "ordered events at {operation}"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            ce_before,
            "CE work at {operation}"
        );

        run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(
                MetadosisContract::new(storage).get_wwd_status(wwd).unwrap(),
                status::FAILED
            );
        });
        assert_eq!(
            provider.storage, clean_storage,
            "MissedOffering retry storage diverged at {operation}"
        );
        assert_eq!(
            provider.events, clean_events,
            "MissedOffering retry events diverged at {operation}"
        );
        assert_eq!(
            provider.get_ordered_events(),
            clean_ordered_events.as_slice(),
            "MissedOffering retry ordered events diverged at {operation}"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            clean_ce_work,
            "MissedOffering retry CE work diverged at {operation}"
        );
        end_persistent_active_scope(&mut provider, &scope);
    }
}

#[test]
fn missed_offering_rolls_back_a_ce_lookup_failure_after_promis_then_retries_once() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0808);
    let later_carry = U256::from(7);
    let formed_limit = U256::from(100);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end =
        seed_missed_offering_day(&mut provider, wwd, formed_limit, U256::ZERO, later_carry);
    let parent_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let tree = Arc::new(FailOncePartitionLookup {
        parent_root,
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
    let ce_before = scope.ce_work_checkpoint().unwrap();
    let error = run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::TreeUnavailable(_)
    ));
    assert_eq!(tree.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    assert_eq!(scope.ce_work_checkpoint().unwrap(), ce_before);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FORMING);
        assert!(metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .is_none());
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            later_carry
        );
    });

    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();
    assert_eq!(tree.calls.load(Ordering::SeqCst), 2);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            later_carry + formed_limit
        );
        assert_eq!(
            metadosis
                .read_missed_offering_receipt(wwd)
                .unwrap()
                .unwrap()
                .retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );
    });
    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn missed_offering_terminalizes_a_day_that_never_formed_its_limit() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0803);
    let carry_over = U256::from(11);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end = seed_unformed_missed_offering_day(&mut provider, wwd, carry_over);
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);

    run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        let receipt = metadosis
            .read_missed_offering_receipt(wwd)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, U256::ZERO);
        assert_eq!(receipt.carry_over_before, carry_over);
        assert_eq!(receipt.carry_over_after, carry_over);
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            carry_over
        );
    });

    end_persistent_active_scope(&mut provider, &scope);
}

#[test]
fn missed_offering_rejects_a_day_limit_with_no_formation() {
    let wwd = outbe_primitives::time::WorldwideDay::new(2026_0804);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let offering_end = seed_unformed_missed_offering_day(&mut provider, wwd, U256::ZERO);
    StorageHandle::enter(&mut provider, |storage| {
        MetadosisContract::new(storage)
            .worldwide_days
            .entry(wwd)
            .metadosis_limit_amount()
            .write(U256::from(5))
            .unwrap();
    });
    let (scope, _parent) = begin_persistent_active_scope(&mut provider);

    let error = run_missed_offering_command(&mut provider, &scope, 2, offering_end).unwrap_err();
    assert!(
        format!("{error:?}").contains("has a day limit with no formation"),
        "unexpected error: {error:?}"
    );

    end_persistent_active_scope(&mut provider, &scope);
}
