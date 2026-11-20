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
