use super::*;

#[test]
fn oracle_owner_mutations_roll_back_every_partial_write_boundary() {
    for (label, fixture, mutation) in [
        (
            "write_snapshot",
            seed_native_oracle as OracleFixture,
            write_snapshot_mutation as OracleMutation,
        ),
        (
            "store_worldwide_day_vwap_snapshot",
            seed_native_oracle_with_snapshot,
            store_wwd_snapshot_mutation,
        ),
        (
            "finalize_utc_day_vwap",
            seed_native_oracle_with_snapshot,
            finalize_utc_day_mutation,
        ),
        (
            "store_scurve_entry",
            seed_native_oracle,
            store_scurve_mutation,
        ),
        (
            "process_daily_scurve",
            seed_native_oracle_with_peak_history,
            process_scurve_mutation,
        ),
    ] {
        assert_oracle_mutation_is_atomic(label, fixture, mutation);
    }
}
#[test]
fn scurve_count_overflow_rejects_before_any_owner_write() {
    with_bare_oracle(|_storage, oracle| {
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle.scurve_count.write(u32::MAX).unwrap();

        assert!(crate::scurve::store_scurve_entry(
            oracle,
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
    });
}
