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
        metadosis.set_metadosis_limit(wwd, U256::ONE).unwrap();

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

        // Tribute pricing reads the snapshot that ResolveForming stored.
        let pricing = outbe_oracle::api::tribute_pricing_inputs(
            storage.clone(),
            outbe_oracle::constants::DAY_TYPE_ISO,
            outbe_oracle::constants::DAY_TYPE_ISO,
            wwd,
        )
        .unwrap()
        .expect("the COEN/840 pair is registered");
        assert_eq!(pricing.issuance_wwd_vwap_minor, U256::from(110u64));
        assert_eq!(pricing.reference_wwd_vwap_minor, U256::from(110u64));

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(wwd).unwrap(),
            status::LOOKBACK_DELAY
        );

        run_begin_block(storage.clone(), 3, offering_entry);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::OFFERING);

        let tribute = TributeContract::new(storage.clone());
        assert!(!tribute.is_day_sealed(wwd).unwrap());

        run_begin_block(storage.clone(), 4, offering_end);

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::WAITING);
        let tribute = TributeContract::new(storage);
        assert!(tribute.is_day_sealed(wwd).unwrap());
    });
}

/// What one day settles to: the Lysis and Desis limits and the warehouse balance after it.
#[derive(Debug, Eq, PartialEq)]
struct DaySettlement {
    lysis_limit_minor: U256,
    desis_limit_minor: U256,
    brief_has_limit: u8,
    unallocated_minor: U256,
}

/// Run one day from Forming to settlement with its COEN/840 VWAP moving from
/// `previous_vwap` to `current_vwap`.
fn settle_day_after_vwap_move(previous_vwap: u64, current_vwap: u64) -> DaySettlement {
    let wwd = outbe_primitives::time::WorldwideDay::new(20260304u32);
    let previous_wwd = wwd.previous_date_key();
    let forming_start = wwd.start_timestamp();
    let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let previous_forming_start = previous_wwd.start_timestamp();
    let previous_forming_end = previous_forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let offering_entry = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
    let offering_end = offering_entry + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let scheduled = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let day_limit = U256::from(1_000u64);
    let nominal = U256::from(1_000u64);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);
    install_test_enclaves();
    StorageHandle::enter(&mut provider, |storage| {
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
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();
        TributeContract::new(storage.clone()).seal_day(wwd).unwrap();

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .write_snapshot(
                previous_forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(previous_vwap), U256::from(1u64))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + SECONDS_PER_HOUR,
                &[(pair, U256::from(current_vwap), U256::from(1u64))],
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
        run_begin_block(storage.clone(), 4, offering_end);
        issue_one_tribute(
            &storage,
            FixtureTribute {
                owner: address!("7500000000000000000000000000000000000075"),
                wwd,
                nominal,
            },
        );
        super::arm_reference_price(&storage, scheduled);
    });
    run_start_command(&mut provider, 5, scheduled).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(metadosis.get_wwd_status(wwd).unwrap(), status::COMPLETED);
        let detail = metadosis
            .read_lysis_completion(wwd)
            .unwrap()
            .expect("the day completes through Lysis");
        let (lysis_limit_minor, desis_limit_minor) = (detail.lysis_limit, detail.desis_limit);
        let desis = storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.pending_desis_limit_minor.read(&wwd).unwrap(),
            desis_limit_minor
        );
        DaySettlement {
            lysis_limit_minor,
            desis_limit_minor,
            brief_has_limit: desis.brief_has_limit.read(&wwd).unwrap(),
            unallocated_minor: PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
        }
    })
}

#[test]
fn a_falling_vwap_day_gets_the_same_limits_as_a_rising_one() {
    let rising = settle_day_after_vwap_move(100, 120);
    let falling = settle_day_after_vwap_move(120, 100);
    let flat = settle_day_after_vwap_move(100, 100);

    // min(floor(1000 * 32 / 100), 1000) = 320 and min(1000 - 320, 1000 - 320 + carry) = 680.
    assert_eq!(rising.lysis_limit_minor, U256::from(320u64));
    assert_eq!(rising.desis_limit_minor, U256::from(680u64));
    assert_eq!(rising.brief_has_limit, 1);
    assert_eq!(falling, rising);
    assert_eq!(flat, rising);
}
