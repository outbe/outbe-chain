use outbe_gemfactory::GemTypes;
use outbe_protocol_benchmarks::{run_scenario, scenarios::gem::GemScenario, GasLedger, RunConfig};

const CONFIG: RunConfig = RunConfig {
    samples: 3,
    warmups: 0,
};

#[test]
fn every_direct_gem_creation_branch_has_a_canonical_postcondition() {
    for gem_type in [
        GemTypes::Genesis,
        GemTypes::Validator,
        GemTypes::Sra,
        GemTypes::Wallet,
        GemTypes::Cca,
    ] {
        let report = run_scenario(&GemScenario::direct(gem_type), CONFIG).unwrap();

        assert_eq!(report.postconditions["gem.created"], "true");
        assert_eq!(
            report.postconditions["gem.type"],
            (gem_type as u8).to_string()
        );
        assert!(report.gas_totals[&GasLedger::SystemInternal] > 0);
        assert!(!report.storage.is_empty());
        assert!(!report.events.is_empty());
    }
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
        merchant.postconditions["gem.type"],
        (GemTypes::Merchant as u8).to_string()
    );
    assert_eq!(
        merchant.postconditions["gem_position.capacity_drained"],
        "true"
    );
}
