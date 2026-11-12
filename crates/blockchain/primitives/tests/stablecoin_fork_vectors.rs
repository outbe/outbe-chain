use outbe_primitives::stablecoin_fork::stablecoin_v1_budget_for_measurement;

#[test]
fn benchmark_margin_rounds_up_and_detects_overflow() {
    assert_eq!(stablecoin_v1_budget_for_measurement(0), Some(0));
    assert_eq!(stablecoin_v1_budget_for_measurement(8_000), Some(10_000));
    assert_eq!(stablecoin_v1_budget_for_measurement(8_001), Some(20_000));
    assert_eq!(stablecoin_v1_budget_for_measurement(u64::MAX), None);
}
