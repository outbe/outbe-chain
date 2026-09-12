use super::*;

#[test]
fn test_offering_entry_captures_vwap_unblocks_and_exit_reblocks() {
    with_storage(|storage| {
        let wwd_raw = 20260302u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair, U256::from(110u64), U256::from(1u64))],
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
        let (_, _, bases, quotes, vwaps, _) = oracle.get_worldwide_day_vwap_snapshot(wwd).unwrap();
        assert_eq!(bases, vec![outbe_oracle::api::COEN_ASSET]);
        assert_eq!(quotes, vec![outbe_oracle::api::currency_address(840)]);
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

#[test]
fn test_missing_previous_vwap_results_in_red_day() {
    with_storage(|storage| {
        let wwd_raw = 20260303u32;
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair, U256::from(110u64), U256::from(1u64))],
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
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + 30 * SECONDS_PER_HOUR,
                &[(pair, U256::from(100u64), U256::from(1u64))],
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
        let wwd = outbe_primitives::time::WorldwideDay::new(wwd_raw);
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

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(100u64), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(120u64), U256::from(1u64))],
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
