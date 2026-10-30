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
            seed_native_oracle_with_covered_day,
            finalize_utc_day_mutation,
        ),
        (
            "store_scurve_entry",
            seed_native_oracle,
            store_scurve_mutation,
        ),
    ] {
        assert_oracle_mutation_is_atomic(label, fixture, mutation);
    }
}
#[test]
fn scurve_len_overflow_rejects_before_any_owner_write() {
    with_bare_oracle(|_storage, oracle| {
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        let pair = pair_key(COEN, usd());
        oracle.scurve_len.write(&pair, u64::MAX).unwrap();

        assert!(crate::scurve::store_scurve_entry(
            oracle,
            pair,
            outbe_primitives::time::WorldwideDay::from_timestamp(ATOMIC_DAY_START),
            coen_iso(125),
        )
        .is_err());
        assert_eq!(oracle.scurve_len.read(&pair).unwrap(), u64::MAX);
        let slot = (u64::MAX - 1) % crate::scurve::SCURVE_RING_SLOTS;
        assert_eq!(
            oracle
                .scurve_peak_price
                .get_nested(&pair)
                .read(&slot)
                .unwrap(),
            U256::ZERO
        );
    });
}
