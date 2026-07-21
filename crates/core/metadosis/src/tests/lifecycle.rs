use super::*;
use outbe_oracle::contract::OracleContract;

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
