use outbe_protocol_benchmarks::{
    run_scenario, scenarios::stablecoin::StablecoinScenario, GasLedger, RunConfig,
};

const CONFIG: RunConfig = RunConfig {
    samples: 3,
    warmups: 0,
};

#[test]
fn stablecoin_reservation_and_approved_creation_have_separate_gas_ledgers() {
    let reserve = run_scenario(&StablecoinScenario::reserve(), CONFIG).unwrap();
    assert_eq!(reserve.postconditions["stablecoin.reserved"], "true");
    assert!(reserve.gas_totals[&GasLedger::SystemInternal] > 0);

    let approved = run_scenario(&StablecoinScenario::approved(), CONFIG).unwrap();
    assert_eq!(approved.postconditions["stablecoin.created"], "true");
    assert_eq!(
        approved.postconditions["stablecoin.identity_matches"],
        "true"
    );
    assert!(approved.gas_totals[&GasLedger::SystemInternal] > 0);
    assert!(!approved.events.is_empty());
}
