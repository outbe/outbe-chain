use outbe_protocol_benchmarks::{
    run_scenario, scenarios::confidential::ConfidentialScenario, CryptoMode, GasLedger, RunConfig,
};

#[test]
fn confidential_creation_paths_use_real_in_process_crypto_and_validate_balances() {
    for scenario in [
        ConfidentialScenario::promis(),
        ConfidentialScenario::gratis(),
        ConfidentialScenario::gratis_with_fidelity(),
        ConfidentialScenario::gratisfactory(),
    ] {
        let report = run_scenario(
            &scenario,
            RunConfig {
                samples: 3,
                warmups: 0,
            },
        )
        .unwrap();

        assert_eq!(report.metadata.crypto_mode, CryptoMode::PortableInProcess);
        assert_eq!(
            report.postconditions["confidential.balance_matches"],
            "true"
        );
        assert!(report.gas_totals[&GasLedger::SystemInternal] > 0);
        assert!(!report.storage.is_empty());
        assert!(!report.events.is_empty());
    }
}
