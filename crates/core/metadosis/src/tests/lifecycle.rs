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

        let promis = PromisLimitContract::new(storage);
        assert_eq!(promis.get_total_unallocated().unwrap(), day_limit);
    });
}

#[test]
fn test_ready_processing_lysis_failure_propagates_and_leaves_day_unsettled() {
    with_storage(|storage| {
        let body_storage = std::sync::Arc::new(outbe_offchain_storage::MemoryStorage::new());
        let body_reader: outbe_offchain_storage::StorageReaderHandle = body_storage.clone();
        let body_writer: outbe_offchain_storage::StorageWriterHandle = body_storage;
        let tribute_bodies = outbe_tribute::TributeRepositoryReader::new(body_reader.clone());
        let tribute_writer =
            outbe_tribute::TributeRepositoryWriter::new(body_reader.clone(), body_writer.clone());
        let nod_bodies = outbe_nod::NodRepositoryReader::new(body_reader.clone());
        let nod_writer = outbe_nod::NodRepositoryWriter::new(body_reader, body_writer);
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
        let token_id = U256::from_be_bytes(alloy_primitives::keccak256([0x01, 0x02, 0x03]).0);

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
            token_id,
            owner,
            worldwide_day: wwd,
            issuance_amount_minor: nominal,
            issuance_currency: 1,
            nominal_amount_minor: nominal,
            reference_currency: 840,
            exclude_from_intex_issuance: false,
            tribute_price_minor: U256::ZERO,
        };
        tribute.issue(&tribute_bodies, &tribute_body).unwrap();
        tribute_writer.put(&tribute_body).unwrap();
        tribute.seal_day(wwd).unwrap();

        // Pre-issue a NOD with the same (owner, worldwide_day) tuple the lysis
        // run will produce, so the second issue collides on nod_id and lysis
        // fails. A lysis failure on a day that already passed FORMING/OFFERING is
        // genuine state corruption, so `process_metadosis` propagates the error
        // out of the begin-zone system transaction instead of silently retiring
        // the day. The test asserts the error surfaces and the day is left
        // unsettled (still READY, limit not routed to PROMIS).
        let nod_id = outbe_nod::NodContract::generate_nod_id(owner, wwd);
        let floor_price_minor = U256::from(1u64);
        nod_writer
            .put_nod(&outbe_nod::NodItemState {
                nod_id,
                owner,
                gratis_load_minor: U256::from(1u64),
                worldwide_day: wwd,
                league_id: 1,
                floor_price_minor,
                bucket_key: outbe_nod::NodContract::bucket_key(wwd, floor_price_minor),
                cost_amount_minor: U256::from(1u64),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: 0,
            })
            .unwrap();

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                2,
                scheduled + SECONDS_PER_HOUR,
                outbe_primitives::chain::CHAIN_ID,
            ),
            storage.clone(),
        );
        let result = crate::runtime::start_metadosis(&ctx, &tribute_bodies, &nod_bodies);
        assert!(
            result.is_err(),
            "lysis failure must propagate out of the begin-zone system transaction"
        );

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
fn no_tributes_green_day_clears_started_auction() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at(
        outbe_desis::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 64]),
    );
    StorageHandle::enter(&mut storage, |storage| {
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

        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()
            .unwrap();

        assert!(outbe_desis::api::dispatch_stage_start(
            storage.clone(),
            auction_ts,
            U256::from(10u64).pow(U256::from(18u64)),
        )
        .unwrap());
        assert!(
            outbe_desis::api::dispatch_stage_reveal(storage.clone(), auction_ts, true).unwrap()
        );

        run_begin_block(storage.clone(), 2, auction_ts + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);

        let promis = PromisLimitContract::new(storage);
        assert!(promis.get_total_unallocated().unwrap() < day_limit);
    });
}

#[test]
fn no_day_limit_green_day_still_empty_clears_started_auction() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_at(
        outbe_desis::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 64]),
    );
    StorageHandle::enter(&mut storage, |storage| {
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

        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()
            .unwrap();

        assert!(outbe_desis::api::dispatch_stage_start(
            storage.clone(),
            auction_ts,
            U256::from(10u64).pow(U256::from(18u64)),
        )
        .unwrap());
        assert!(
            outbe_desis::api::dispatch_stage_reveal(storage.clone(), auction_ts, true).unwrap()
        );

        run_begin_block(storage.clone(), 2, auction_ts + SECONDS_PER_HOUR);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::FAILED);

        let series = timestamp_to_date_key(auction_ts);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(desis.clearing_initiated.read(&series).unwrap(), 1);
        assert_eq!(desis.pending_supply_intex.read(&series).unwrap(), 0);
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
        with_empty_body_readers(|tribute_bodies, nod_bodies| {
            crate::runtime::start_metadosis(&ctx, tribute_bodies, nod_bodies)
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
fn intex_reveal_dispatched_on_mid_offering_tick() {
    use alloy_sol_types::SolEvent;
    use outbe_desis::precompile::IDesis;
    // Offering spans two daily ticks (48h). Reveal must dispatch on the second
    // (mid-offering) tick so it lands separately from clearing at READY; otherwise
    // reveal and clearing collide on one tick. Probed via the best-effort
    // AuctionDispatchFailed event (now emitted by Desis): Desis has no code in tests,
    // so every dispatch attempt fails and emits one.
    const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let desis_addr = outbe_primitives::addresses::DESIS_ADDRESS;
    let fail_sig = IDesis::AuctionDispatchFailed::SIGNATURE_HASH;
    let base_ts = crate::runtime::date_key_to_timestamp(20260601);

    // Track one wwd by its date key; many wwds dispatch each tick, so filter the
    // event by its indexed seriesId (the wwd's scheduled-process date) to isolate it.
    let wwd_key: u32 = 20260601;
    let mut stages: Vec<Option<String>> = Vec::new();
    for k in 0..7u64 {
        storage.clear_events(desis_addr);
        let ts = base_ts + k * SECONDS_PER_DAY;
        StorageHandle::enter(&mut storage, |storage| {
            run_begin_block(storage.clone(), k + 1, ts);
        });
        // Desis sees seriesId = timestamp_to_date_key(scheduled_process_time).
        let target_series = StorageHandle::enter(&mut storage, |storage| {
            let metadosis = MetadosisContract::new(storage);
            let scheduled = metadosis
                .worldwide_days
                .entry(outbe_common::WorldwideDay::from(wwd_key))
                .scheduled_process_time()
                .read()
                .unwrap_or(0);
            timestamp_to_date_key(scheduled)
        });
        let stage = storage.get_events(desis_addr).iter().find_map(|log| {
            if log.topics().first() != Some(&fail_sig) {
                return None;
            }
            let ev = IDesis::AuctionDispatchFailed::decode_log_data(log).ok()?;
            (ev.seriesId == target_series).then_some(ev.stage)
        });
        println!(
            "tick {k}: date={} stage={stage:?}",
            timestamp_to_date_key(ts)
        );
        stages.push(stage);
    }

    // k0/k1 FORMING, k2 offering entry (start), k3 mid-offering (reveal), k4 READY.
    assert_eq!(
        stages[2].as_deref(),
        Some("auction_stage_start"),
        "start must dispatch on the offering-entry tick: {stages:?}"
    );
    assert_eq!(
        stages[3].as_deref(),
        Some("auction_stage_reveal"),
        "reveal must dispatch on the mid-offering tick, not bundled with clearing: {stages:?}"
    );
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
