use super::*;

#[test]
fn ocomp_pre_admission_reads_priced_currencies_and_bounded_counts() {
    let timestamp = 1_753_315_200_u64;
    with_storage_at(timestamp, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        let wwd = outbe_primitives::time::WorldwideDay::from_timestamp(timestamp);
        let last_closed = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(timestamp),
        );
        let last_closed_start = outbe_primitives::time::date_key_to_utc_timestamp(last_closed);
        let last_closed_price = coen_iso(125);

        let uninitialized = crate::api::ocomp_pre_admission_projection(storage.clone()).unwrap();
        assert!(!uninitialized.profile_ready);
        assert_eq!(uninitialized.oracle_state_version, 0);

        crate::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        let forming_start = wwd.start_timestamp();
        oracle
            .write_snapshot(
                forming_start + 100,
                &[(pair_key(COEN, usd()), last_closed_price, coen_iso(1))],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(wwd, forming_start, forming_start + 50 * 60 * 60)
            .unwrap();
        oracle.finalize_utc_day_vwap(last_closed).unwrap();
        crate::scurve::store_scurve_entry(
            &mut oracle,
            pair_key(COEN, usd()),
            last_closed_start,
            coen_iso(200),
        )
        .unwrap();

        let registered_pairs = oracle.pair_count.read().unwrap();
        let closed = crate::api::ocomp_pre_admission_projection(storage.clone()).unwrap();
        assert!(closed.profile_ready);
        assert_eq!(closed.oracle_state_version, 5);
        // The opening bound is now the registry size, not a per-day entry count.
        assert_eq!(closed.wwd_pair_entries, registered_pairs);
        assert_eq!(closed.active_scurve_entries, 1);

        // Only a currency whose own pair closed on the UTC day is present.
        assert_eq!(
            crate::api::priced_reference_currencies(storage.clone(), last_closed).unwrap(),
            vec![(crate::constants::DAY_TYPE_ISO, last_closed_price)]
        );
        // A day with no price omits the day-type row rather than substituting one.
        let next_day = outbe_primitives::time::timestamp_to_date_key(timestamp);
        assert!(crate::api::priced_reference_currencies(storage, next_day)
            .unwrap()
            .is_empty());
    });
}

#[test]
fn ocomp_oracle_profile_initialization_is_exact_and_idempotent() {
    with_storage(|storage| {
        assert!(crate::api::initialize_fresh_ocomp_profile(storage.clone()).is_err());

        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();

        assert!(oracle.ocomp_profile_ready.read().unwrap());
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), 1);
    });
}

#[test]
fn ocomp_state_version_overflow_rejects_before_oracle_mutation() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
        oracle.ocomp_state_version.write(u64::MAX).unwrap();

        assert!(oracle
            .write_snapshot(1_000, &[(pair_key(COEN, usd()), coen_iso(10), coen_iso(1))],)
            .is_err());
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_eq!(oracle.snapshot_pair_count.read(&0).unwrap(), 0);
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), u64::MAX);
    });
}
#[test]
fn ocomp_oracle_owner_mutations_roll_back_every_partial_write_boundary() {
    for (label, fixture, mutation) in [
        (
            "write_snapshot",
            seed_ocomp_oracle as OracleFixture,
            write_snapshot_mutation as OracleMutation,
        ),
        (
            "store_worldwide_day_vwap_snapshot",
            seed_ocomp_oracle_with_snapshot,
            store_wwd_snapshot_mutation,
        ),
        (
            "finalize_utc_day_vwap",
            seed_ocomp_oracle_with_snapshot,
            finalize_utc_day_mutation,
        ),
        (
            "store_scurve_entry",
            seed_ocomp_oracle,
            store_scurve_mutation,
        ),
        (
            "process_daily_scurve",
            seed_ocomp_oracle_with_peak_history,
            process_scurve_mutation,
        ),
    ] {
        assert_oracle_mutation_is_atomic(label, fixture, mutation);
    }
}
#[test]
fn prefork_oracle_event_failures_preserve_historical_best_effort_mutations() {
    let mut finalized = run_prefork_with_last_mutation_failure(
        seed_prefork_oracle_with_snapshot,
        finalize_utc_day_mutation,
    );
    StorageHandle::enter(&mut finalized, |storage| {
        let oracle = OracleContract::new(storage);
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(
                    outbe_primitives::time::timestamp_to_date_key(ATOMIC_DAY_START),
                    oracle.pair_index_of(pair_key(COEN, usd())).unwrap(),
                )
                .unwrap(),
            Some(coen_iso(125))
        );
        assert!(!oracle.ocomp_profile_ready.read().unwrap());
    });

    let mut processed = run_prefork_with_last_mutation_failure(
        seed_prefork_oracle_with_peak_history,
        process_scurve_mutation,
    );
    StorageHandle::enter(&mut processed, |storage| {
        let oracle = OracleContract::new(storage);
        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(
            oracle.scurve_peak_day.read(&0).unwrap(),
            SCURVE_CURRENT_DAY - 2 * crate::scurve::DAY_SECONDS
        );
        assert!(!oracle.ocomp_profile_ready.read().unwrap());
    });
}

#[test]
fn scurve_count_overflow_rejects_before_any_owner_write() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
        oracle.scurve_count.write(u32::MAX).unwrap();
        let version_before = oracle.ocomp_state_version.read().unwrap();

        assert!(crate::scurve::store_scurve_entry(
            &mut oracle,
            pair_key(COEN, usd()),
            ATOMIC_DAY_START,
            coen_iso(125),
        )
        .is_err());
        assert_eq!(oracle.scurve_count.read().unwrap(), u32::MAX);
        assert_eq!(
            oracle.scurve_pair.read_pair(&u32::MAX).unwrap(),
            AddressPair::ZERO
        );
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), version_before);
    });
}
