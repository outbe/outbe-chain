use outbe_protocol_benchmarks::{run_scenario, scenarios::gem::GemScenario, GasLedger, RunConfig};

const CONFIG: RunConfig = RunConfig {
    samples: 3,
    warmups: 0,
};

#[test]
fn direct_gem_creation_has_a_canonical_postcondition() {
    let report = run_scenario(&GemScenario::direct(), CONFIG).unwrap();

    assert_eq!(report.postconditions["gem.created"], "true");
    assert!(report.gas_totals[&GasLedger::SystemInternal] > 0);
    assert!(!report.storage.is_empty());
    assert!(!report.events.is_empty());
}

#[test]
fn gem_position_and_merchant_gem_cover_the_marginal_parent_frames() {
    let position = run_scenario(&GemScenario::position(), CONFIG).unwrap();
    assert_eq!(position.postconditions["gem_position.created"], "true");
    assert_eq!(
        position.postconditions["gem_position.owner_matches"],
        "true"
    );

    let merchant = run_scenario(&GemScenario::merchant(), CONFIG).unwrap();
    assert_eq!(merchant.postconditions["gem.created"], "true");
    assert_eq!(
        merchant.postconditions["gem_position.capacity_drained"],
        "true"
    );
}
