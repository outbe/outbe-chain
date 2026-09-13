use crate::world::rpc::*;

#[test]
fn zerofee_rollover_wait_budget_covers_the_canonical_distance_to_boundary() {
    assert_eq!(zerofee_rollover_wait_budget_secs(1_787_615_641), 419);
    assert_eq!(zerofee_rollover_wait_budget_secs(1_787_615_950), 150);
}
