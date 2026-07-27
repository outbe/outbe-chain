use super::*;
use alloy_sol_types::SolEvent;
use outbe_nod::NodContract;
use outbe_oracle::contract::OracleContract;

fn arm_ocomp_request_profile(storage: &StorageHandle) {
    let mut profile = super::ocomp_storage::request_profile();
    profile.chain_id = CHAIN_ID;
    MetadosisContract::new(storage.clone())
        .initialize_ocomp_request_profile(&profile, &crate::ocomp::schema::poc_schema_limits())
        .unwrap();
}

fn create_waiting_day(
    storage: &StorageHandle,
    wwd: outbe_common::WorldwideDay,
    dtype: u8,
    day_limit: U256,
) -> u64 {
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
    metadosis.set_wwd_day_type(wwd, dtype).unwrap();
    metadosis
        .worldwide_days
        .entry(wwd)
        .status()
        .write(status::WAITING)
        .unwrap();
    metadosis.set_metadosis_limit(wwd, day_limit).unwrap();
    metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()
        .unwrap()
}

fn issue_one_tribute_and_run_metadosis(
    storage: &StorageHandle,
    wwd: outbe_common::WorldwideDay,
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
        crate::runtime::start_metadosis(&ctx, scope, parent).unwrap();
    });
}

fn issue_one_tribute_in_scope(
    storage: &StorageHandle,
    scope: &ExecutionScope,
    parent: &TestParent,
    owner: Address,
    wwd: outbe_common::WorldwideDay,
    nominal: U256,
) {
    let mut tribute = TributeContract::new(storage.clone());
    tribute.initialize_fresh_ocomp_profile().unwrap();
    tribute.unseal_day(wwd).unwrap();
    tribute
        .issue(
            scope,
            parent,
            &TributeData {
                tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                owner,
                worldwide_day: wwd,
                issuance_amount_minor: nominal,
                issuance_currency: 840,
                nominal_amount_minor: nominal,
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                tribute_price_minor: U256::from(2),
            },
        )
        .unwrap();
    tribute.seal_day(wwd).unwrap();
}

fn assert_no_ocomp_job(storage: &StorageHandle, wwd: outbe_common::WorldwideDay) {
    let metadosis = MetadosisContract::new(storage.clone());
    let limits = crate::ocomp::schema::poc_schema_limits();
    assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
    assert!(metadosis.ocomp_ready_index.is_empty().unwrap());
    assert!(metadosis
        .ocomp_fsm_states
        .get_bytes(&wwd)
        .is_empty()
        .unwrap());
    assert!(metadosis.ocomp_terminal_intents.is_empty().unwrap());
    assert!(metadosis
        .request_budget_receipt(wwd, &limits)
        .unwrap()
        .is_none());
    assert!(metadosis
        .read_pre_admission_envelope(wwd, &limits)
        .unwrap()
        .is_none());
}

#[test]
fn test_emission_sink_writes_metadosis_limit_for_worldwide_day() {
    with_storage(|storage| {
        let timestamp =
            outbe_common::WorldwideDay::new(20241221).start_timestamp() + 2 * SECONDS_PER_HOUR;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, CHAIN_ID),
            storage.clone(),
        );

        // The terminal sink now writes the limit onto the WorldwideDay record
        // (UTC+14 keyed) for the block timestamp, not a separate UTC-date-key map.
        let wwd = outbe_common::WorldwideDay::from_timestamp(timestamp);

        crate::emission_sink::apply(&ctx, U256::from(123u64)).unwrap();

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            U256::from(123u64)
        );
        // A neighboring day is untouched.
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd.previous_date_key())
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn ocomp_day_limit_formation_takes_carry_over_once_and_late_credit_waits() {
    with_storage(|storage| {
        arm_ocomp_request_profile(&storage);
        let first = outbe_common::WorldwideDay::new(20260725);
        let second = outbe_common::WorldwideDay::new(20260726);
        let third = outbe_common::WorldwideDay::new(20260727);
        let first_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                10,
                first.start_timestamp() + 2 * SECONDS_PER_HOUR,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        let second_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                20,
                second.start_timestamp() + 2 * SECONDS_PER_HOUR,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        let third_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                30,
                third.start_timestamp() + 2 * SECONDS_PER_HOUR,
                CHAIN_ID,
            ),
            storage.clone(),
        );

        let mut promis = PromisLimitContract::new(storage.clone());
        promis.checked_add_carry_over(U256::from(30)).unwrap();
        crate::emission_sink::apply(&first_ctx, U256::from(100)).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let first_day = metadosis.worldwide_days.entry(first);
        let first_formation = metadosis.ocomp_day_limit_formation(first).unwrap().unwrap();
        assert_eq!(first_formation.carry_over_taken, U256::from(30));
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(130)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::ZERO);

        promis.checked_add_carry_over(U256::from(7)).unwrap();
        crate::emission_sink::apply(&first_ctx, U256::from(100)).unwrap();
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(130)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(7));
        assert!(crate::emission_sink::apply(&first_ctx, U256::from(101)).is_err());
        assert!(MetadosisContract::new(storage.clone())
            .set_metadosis_limit(first, U256::from(999))
            .is_err());
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(130)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(7));

        crate::emission_sink::apply(&second_ctx, U256::from(200)).unwrap();
        let second_day = metadosis.worldwide_days.entry(second);
        let second_formation = metadosis
            .ocomp_day_limit_formation(second)
            .unwrap()
            .unwrap();
        assert_eq!(second_formation.carry_over_taken, U256::from(7));
        assert_eq!(
            second_day.metadosis_limit_amount().read().unwrap(),
            U256::from(207)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::ZERO);

        crate::emission_sink::apply(&third_ctx, U256::from(50)).unwrap();
        let third_formation = metadosis.ocomp_day_limit_formation(third).unwrap().unwrap();
        assert_eq!(third_formation.base_limit, U256::from(50));
        assert_eq!(third_formation.carry_over_taken, U256::ZERO);
        assert_eq!(third_formation.day_limit, U256::from(50));

        MetadosisContract::new(storage.clone())
            .delete_worldwide_day(third)
            .unwrap();
        assert!(MetadosisContract::new(storage)
            .ocomp_day_limit_formation(third)
            .unwrap()
            .is_none());
    });
}

#[test]
fn ocomp_day_limit_overflow_and_every_mutation_failure_are_atomic() {
    fn seed(provider: &mut HashMapStorageProvider, carry_over: U256) {
        StorageHandle::enter(provider, |storage| {
            arm_ocomp_request_profile(&storage);
            PromisLimitContract::new(storage)
                .checked_add_carry_over(carry_over)
                .unwrap();
        });
    }

    fn apply_limit(
        provider: &mut HashMapStorageProvider,
        base_limit: U256,
    ) -> outbe_primitives::error::Result<U256> {
        let wwd = outbe_common::WorldwideDay::new(20260727);
        StorageHandle::enter(provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(
                    30,
                    wwd.start_timestamp() + 2 * SECONDS_PER_HOUR,
                    CHAIN_ID,
                ),
                storage,
            );
            crate::emission_sink::apply(&ctx, base_limit)
        })
    }

    let mut overflow = HashMapStorageProvider::new(CHAIN_ID);
    seed(&mut overflow, U256::from(1));
    let before_storage = overflow.storage.clone();
    let before_events = overflow.events.clone();
    assert!(apply_limit(&mut overflow, U256::MAX).is_err());
    assert_eq!(overflow.storage, before_storage);
    assert_eq!(overflow.events, before_events);
    StorageHandle::enter(&mut overflow, |storage| {
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(1)
        );
        assert!(MetadosisContract::new(storage)
            .ocomp_day_limit_formation(outbe_common::WorldwideDay::new(20260727))
            .unwrap()
            .is_none());
    });

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    seed(&mut probe, U256::from(9));
    probe.fail_after_mutation_at(usize::MAX);
    apply_limit(&mut probe, U256::from(100)).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 3,
        "formation must mutate Promis, Metadosis and events"
    );
    let event = IMetadosis::OcompDayLimitFormed::decode_log(
        probe.get_ordered_events().last().expect("formation event"),
    )
    .unwrap();
    assert_eq!(event.data.worldwideDay, 20260727);
    assert_eq!(event.data.baseLimit, U256::from(100));
    assert_eq!(event.data.carryOverBefore, U256::from(9));
    assert_eq!(event.data.carryOverTaken, U256::from(9));
    assert_eq!(event.data.carryOverAfter, U256::ZERO);
    assert_eq!(event.data.formedDayLimit, U256::from(109));
    let replay_event_count = probe.get_ordered_events().len();
    apply_limit(&mut probe, U256::from(100)).unwrap();
    assert_eq!(probe.get_ordered_events().len(), replay_event_count);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed(&mut provider, U256::from(9));
        let before_storage = provider.storage.clone();
        let before_events = provider.events.clone();
        provider.fail_after_mutation_at(operation);

        assert!(apply_limit(&mut provider, U256::from(100)).is_err());
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, before_storage);
        assert_eq!(provider.events, before_events);

        apply_limit(&mut provider, U256::from(100)).unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            let formed = MetadosisContract::new(storage.clone())
                .ocomp_day_limit_formation(outbe_common::WorldwideDay::new(20260727))
                .unwrap()
                .unwrap();
            assert_eq!(formed.base_limit, U256::from(100));
            assert_eq!(formed.carry_over_taken, U256::from(9));
            assert_eq!(formed.day_limit, U256::from(109));
            assert_eq!(
                PromisLimitContract::new(storage)
                    .get_total_unallocated()
                    .unwrap(),
                U256::ZERO
            );
        });
    }
}

#[test]
fn test_cold_start_creates_utc_day_and_current_utc_plus_14_day() {
    with_storage(|storage| {
        let timestamp =
            outbe_common::WorldwideDay::new(20260302).start_timestamp() + 2 * SECONDS_PER_HOUR;
        run_begin_block(storage.clone(), 1, timestamp);

        let metadosis = MetadosisContract::new(storage.clone());
        let active = metadosis.active_wwd.read_all().unwrap();
        assert!(active.contains(&20260301u32.into()));
        assert!(active.contains(&20260302u32.into()));
        assert_eq!(
            metadosis.get_bootstrap_end_time().unwrap(),
            timestamp + BOOTSTRAP_DURATION_HOURS * SECONDS_PER_HOUR
        );

        let tribute = TributeContract::new(storage);
        assert!(tribute.is_day_sealed(20260301u32.into()).unwrap());
        assert!(tribute.is_day_sealed(20260302u32.into()).unwrap());
    });
}

#[test]
fn test_cold_start_non_bootstrap_chain_uses_default_schedule_and_no_bootstrap_end_time() {
    with_storage(|storage| {
        let timestamp =
            outbe_common::WorldwideDay::new(20260302).start_timestamp() + 2 * SECONDS_PER_HOUR;
        run_begin_block_with_chain_id(storage.clone(), 1, timestamp, CHAIN_ID);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_bootstrap_end_time().unwrap(), 0);

        let active = metadosis.active_wwd.read_all().unwrap();
        assert!(active.contains(&20260301u32.into()));
        assert!(active.contains(&20260302u32.into()));

        let wwd = 20260302u32;
        let forming_start = outbe_common::WorldwideDay::new(wwd).start_timestamp();
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

#[test]
fn test_offering_entry_captures_vwap_unblocks_and_exit_reblocks() {
    with_storage(|storage| {
        let wwd_raw = 20260302u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
        let previous_wwd = wwd.previous_date_key();
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let previous_forming_start = previous_wwd.start_timestamp();
        let previous_forming_end = previous_forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let offering_entry = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let offering_end = offering_entry + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;

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

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair_id, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair_id, U256::from(110u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(
                previous_wwd,
                previous_forming_start,
                previous_forming_end,
            )
            .unwrap();

        run_begin_block(storage.clone(), 2, forming_end);

        let oracle = OracleContract::new(storage.clone());
        let (_, _, pair_ids, vwaps, _) = oracle.get_worldwide_day_vwap_snapshot(wwd).unwrap();
        assert_eq!(pair_ids, vec![pair_id]);
        assert_eq!(vwaps, vec![U256::from(110u64)]);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(wwd).unwrap(),
            status::LOOKBACK_DELAY
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .previous_vwap()
                .read()
                .unwrap(),
            U256::from(100u64)
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .current_vwap()
                .read()
                .unwrap(),
            U256::from(110u64)
        );
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::GREEN);

        run_begin_block(storage.clone(), 3, offering_entry);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .previous_vwap()
                .read()
                .unwrap(),
            U256::from(100u64)
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .current_vwap()
                .read()
                .unwrap(),
            U256::from(110u64)
        );
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::GREEN);

        let tribute = TributeContract::new(storage.clone());
        assert!(!tribute.is_day_sealed(wwd).unwrap());

        run_begin_block(storage.clone(), 4, offering_end);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::WAITING);
        let tribute = TributeContract::new(storage);
        assert!(tribute.is_day_sealed(wwd).unwrap());
    });
}

/// `advance_active_worldwide_days` (the 12:00 UTC `wwd_advance_noon` Cycle
/// trigger handler) must walk the status machine forward exactly like the
/// midnight path — including the FORMING→OFFERING side effects (tribute day
/// unseal) — but must NOT create a new worldwide day and must NOT settle a
/// READY one; day creation and settlement stay midnight-owned in
/// `start_metadosis`.
#[test]
fn advance_active_worldwide_days_advances_status_without_creating_or_settling() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20260302u32);
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
            crate::runtime::advance_active_worldwide_days(&ctx).unwrap();
        };

        // At the offering-entry edge the day opens and the tribute day
        // unseals — offers stop reverting `not in OFFERING status`.
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
fn test_missing_previous_vwap_results_in_red_day() {
    with_storage(|storage| {
        let wwd_raw = 20260303u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let offering_entry = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;

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

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair_id, U256::from(110u64), U256::from(1u64))],
            )
            .unwrap();

        run_begin_block(storage.clone(), 2, forming_end);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(wwd).unwrap(),
            status::LOOKBACK_DELAY
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .previous_vwap()
                .read()
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .current_vwap()
                .read()
                .unwrap(),
            U256::from(110u64)
        );
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::RED);

        run_begin_block(storage.clone(), 3, offering_entry);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::RED);
    });
}

#[test]
fn test_equal_vwap_results_in_red_day() {
    with_storage(|storage| {
        let wwd_raw = 20260303u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
        let previous_wwd = wwd.previous_date_key();
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let previous_forming_start = previous_wwd.start_timestamp();
        let previous_forming_end = previous_forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let offering_entry = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;

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

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair_id, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair_id, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(
                previous_wwd,
                previous_forming_start,
                previous_forming_end,
            )
            .unwrap();

        run_begin_block(storage.clone(), 2, forming_end);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(wwd).unwrap(),
            status::LOOKBACK_DELAY
        );
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::RED);

        run_begin_block(storage.clone(), 3, offering_entry);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::RED);
    });
}

#[test]
fn test_normal_lifecycle_never_leaves_ready_day_type_unknown() {
    with_storage(|storage| {
        let wwd_raw = 20260304u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
        let previous_wwd = wwd.previous_date_key();
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let previous_forming_start = previous_wwd.start_timestamp();
        let previous_forming_end = previous_forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let offering_entry = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let scheduled = offering_entry
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

        let mut tribute = TributeContract::new(storage.clone());
        tribute.seal_day(wwd).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair_id, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + SECONDS_PER_HOUR,
                &[(pair_id, U256::from(120u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(
                previous_wwd,
                previous_forming_start,
                previous_forming_end,
            )
            .unwrap();

        run_begin_block(storage.clone(), 2, forming_end);
        run_begin_block(storage.clone(), 3, offering_entry);
        run_begin_block(storage.clone(), 4, scheduled);

        let metadosis = MetadosisContract::new(storage);
        assert_ne!(metadosis.get_wwd_day_type(wwd).unwrap(), day_type::UNKNOWN);
    });
}

#[test]
fn test_ready_processing_missing_limit_fails_like_source() {
    with_storage(|storage| {
        let wwd_raw = 20260310u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
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
        metadosis.set_wwd_day_type(wwd, day_type::RED).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
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
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
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
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
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
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
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
        metadosis.set_wwd_day_type(wwd, day_type::RED).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
            .unwrap();
        metadosis.set_metadosis_limit(wwd, U256::ZERO).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);
    });
}

#[test]
fn test_ready_processing_no_tributes_returns_full_limit_to_promis() {
    with_storage(|storage| {
        let wwd_raw = 20260312u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
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
        metadosis.set_wwd_day_type(wwd, day_type::RED).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
            .unwrap();
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);

        // A red day is recorded as a supply-less brief; the limit stays in PROMIS.
        let series = u32::from(wwd);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_supply_promis.read(&series).unwrap(),
            U256::ZERO
        );

        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), day_limit);
    });
}

#[test]
fn active_ocomp_profile_discovers_later_ready_day_after_first_was_indexed() {
    with_storage(|storage| {
        let first_wwd = outbe_common::WorldwideDay::new(2026_0316);
        let second_wwd = outbe_common::WorldwideDay::new(2026_0317);
        let nominal = U256::from(1_000);
        let first_scheduled =
            create_waiting_day(&storage, first_wwd, day_type::GREEN, U256::from(800));
        let second_scheduled =
            create_waiting_day(&storage, second_wwd, day_type::GREEN, U256::from(900));
        let timestamp = first_scheduled.max(second_scheduled) + SECONDS_PER_HOUR;
        arm_ocomp_request_profile(&storage);

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
            crate::runtime::start_metadosis(&first_ctx, scope, parent).unwrap();

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
            crate::runtime::start_metadosis(&second_ctx, scope, parent).unwrap();
        });

        let metadosis = MetadosisContract::new(storage);
        let schema_limits = crate::ocomp::schema::poc_schema_limits();
        let fsm_limits =
            crate::ocomp::request::fsm_limits(&super::ocomp_storage::request_profile());
        let first = metadosis
            .ocomp_fsm_state(first_wwd, &schema_limits, fsm_limits)
            .unwrap()
            .projection();
        let second = metadosis
            .ocomp_fsm_state(second_wwd, &schema_limits, fsm_limits)
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
fn active_ocomp_profile_preserves_the_empty_day_compatibility_branch() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(2026_0313);
        let day_limit = U256::from(777);
        let scheduled = create_waiting_day(&storage, wwd, day_type::RED, day_limit);
        arm_ocomp_request_profile(&storage);

        run_begin_block(storage.clone(), 2, scheduled + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        assert!(!metadosis.active_wwd.read_all().unwrap().contains(&wwd));
        assert!(metadosis.closed_wwd.read_all().unwrap().contains(&wwd));
        assert_no_ocomp_job(&storage, wwd);

        let series = wwd.value();
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_supply_promis.read(&series).unwrap(),
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
fn active_ocomp_profile_preserves_the_populated_zero_limit_branch() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(2026_0314);
        let nominal = U256::from(1_000);
        let scheduled = create_waiting_day(&storage, wwd, day_type::GREEN, U256::ZERO);
        arm_ocomp_request_profile(&storage);

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

        let series = wwd.value();
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
fn active_ocomp_profile_preserves_the_populated_zero_lysis_budget_branch() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(2026_0318);
        let nominal = U256::from(1_000);
        // A red day divides supply by RED_DAY_REDUCTION_COEF. This non-zero
        // day limit therefore produces an exact zero Lysis allocation.
        let day_limit = U256::from(2);
        let scheduled = create_waiting_day(&storage, wwd, day_type::RED, day_limit);
        arm_ocomp_request_profile(&storage);

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

        let series = wwd.value();
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 0);
        assert_eq!(
            desis.pending_supply_promis.read(&series).unwrap(),
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
        let wwd = outbe_common::WorldwideDay::new(2026_0315);
        let nominal = U256::from(1_000);
        let day_limit = U256::from(333);
        let scheduled = create_waiting_day(&storage, wwd, day_type::UNKNOWN, day_limit);
        arm_ocomp_request_profile(&storage);

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

        let series = wwd.value();
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
fn test_ready_processing_lysis_failure_propagates_and_leaves_day_unsettled() {
    with_storage(|storage| {
        let parent = TestParent::empty();
        let scope = ExecutionScope::new();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::ZERO,
                U256::from(3),
            )
            .unwrap();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(alloy_primitives::B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
        begin_block(storage.clone(), &scope).unwrap();
        let wwd_raw = 20260313u32;
        let wwd = outbe_common::WorldwideDay::new(wwd_raw);
        let day_limit = U256::from(5_000u64) * U256::from(10u64).pow(U256::from(18u64));
        let nominal = U256::from(1_000u64) * U256::from(10u64).pow(U256::from(18u64));
        let forming_start = wwd.start_timestamp();
        let scheduled = forming_start
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
            + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
            + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let owner = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let tribute_id = outbe_nod::NodContract::generate_nod_id(owner, wwd).unwrap();

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
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
            .unwrap();
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();

        let mut tribute = TributeContract::new(storage.clone());
        tribute.unseal_day(wwd).unwrap();
        let tribute_body = TributeData {
            tribute_id,
            owner,
            worldwide_day: wwd,
            issuance_amount_minor: nominal,
            issuance_currency: 1,
            nominal_amount_minor: nominal,
            reference_currency: 840,
            exclude_from_intex_issuance: false,
            tribute_price_minor: U256::ZERO,
        };
        tribute.issue(&scope, &parent, &tribute_body).unwrap();
        tribute.seal_day(wwd).unwrap();

        // Pre-issue a NOD with the same (owner, worldwide_day) tuple the lysis
        // run will produce, so the second issue collides on nod_id and lysis
        // fails. A lysis failure on a day that already passed FORMING/OFFERING is
        // genuine state corruption, so `process_metadosis` propagates the error
        // out of the begin-zone system transaction instead of silently retiring
        // the day. The test asserts the error surfaces and the day is left
        // unsettled (still READY, limit not routed to PROMIS).
        let floor_price_minor = U256::from(1u64);
        outbe_nodfactory::api::issue_nod(
            &storage,
            &scope,
            &parent,
            &outbe_nod::NodIssueParams {
                owner,
                gratis_load_minor: U256::from(1u64),
                worldwide_day: wwd,
                league_id: 1,
                floor_price_minor,
                entry_price_minor: U256::from(1u64),
                cost_amount_minor: U256::from(1u64),
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
        let result = crate::runtime::start_metadosis(&ctx, &scope, &parent);
        assert!(
            result.is_err(),
            "lysis failure must propagate out of the begin-zone system transaction"
        );
        end_block(storage.clone(), &scope).unwrap();

        // The error carries the real reason out. `process_metadosis` records the
        // FAILED transition before propagating (observable here because the test
        // harness does not revert; on the production path the propagated error
        // reverts the system tx and rolls this write back). The limit is never
        // routed to PROMIS, and the tribute is untouched.
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);

        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.total_supply().unwrap(), 1);

        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::ZERO);
    });
}

#[test]
fn no_tributes_green_day_briefs_the_full_limit() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20260401u32);
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
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
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

        let series = u32::from(wwd);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_green.read(&series).unwrap(), 1);
        assert_eq!(
            desis.pending_supply_promis.read(&series).unwrap(),
            day_limit
        );

        let promis = PromisLimitContract::new(storage);
        assert_eq!(
            promis.get_total_unallocated().unwrap(),
            U256::ZERO,
            "a green brief takes the whole no-tributes limit"
        );
    });
}

#[test]
fn zero_limit_green_day_dispatches_no_brief() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20260501u32);
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
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
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

        let series = u32::from(wwd);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&series).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        assert_eq!(desis.clearing_initiated.read(&series).unwrap(), 0);
    });
}

#[test]
fn test_events_emitted_for_accumulation_and_lifecycle() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let contract_addr = outbe_primitives::addresses::METADOSIS_ADDRESS;

    StorageHandle::enter(&mut storage, |storage| {
        let timestamp =
            outbe_common::WorldwideDay::new(20260302).start_timestamp() + 2 * SECONDS_PER_HOUR;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, outbe_primitives::chain::CHAIN_ID),
            storage.clone(),
        );
        crate::emission_sink::apply(&ctx, U256::from(10u64)).unwrap();
        with_active_scope(storage, |scope, parent| {
            crate::runtime::start_metadosis(&ctx, scope, parent)
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
fn auction_brief_dispatched_only_on_the_ready_tick() {
    const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
    with_storage(|storage| {
        let wwd_key: u32 = 20260601;
        let base_ts = crate::runtime::date_key_to_timestamp(wwd_key);

        // Block 1 creates the day; seed its limit afterwards so READY processing
        // has something to brief.
        run_begin_block(storage.clone(), 1, base_ts);
        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .set_metadosis_limit(wwd_key.into(), U256::from(777u64))
            .unwrap();
        drop(metadosis);

        // k1 FORMING, k2 offering entry, k3 mid-offering, k4 READY.
        let mut stages = Vec::new();
        for k in 1..5u64 {
            run_begin_block(storage.clone(), k + 1, base_ts + k * SECONDS_PER_DAY);
            let desis = storage.contract::<outbe_desis::schema::DesisContract>();
            stages.push(desis.auction_stage.read(&wwd_key).unwrap());
        }

        let briefed = outbe_desis::schema::AuctionStage::Briefed as u8;
        assert_eq!(
            stages,
            vec![0, 0, 0, briefed],
            "the brief must dispatch on the READY tick only"
        );
    });
}

#[test]
fn test_terminal_day_leaves_active_set() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20260315u32);
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
        metadosis.set_wwd_day_type(wwd, day_type::RED).unwrap();
        metadosis
            .worldwide_days
            .entry(wwd)
            .status()
            .write(status::WAITING)
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
            .get_active_wwd_by_status(status::COMPLETED)
            .unwrap()
            .contains(&wwd));
    });
}
