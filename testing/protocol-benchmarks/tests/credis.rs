use outbe_protocol_benchmarks::{
    run_scenario, scenarios::credis::CredisScenario, CryptoMode, GasLedger, RunConfig,
};

#[test]
fn credis_request_runs_real_confidential_state_and_reports_user_transaction_gas() {
    let report = run_scenario(
        &CredisScenario::request(),
        RunConfig {
            samples: 3,
            warmups: 0,
        },
    )
    .unwrap();

    assert_eq!(report.metadata.crypto_mode, CryptoMode::PortableInProcess);
    assert_eq!(report.postconditions["credis.created"], "true");
    assert_eq!(report.postconditions["credis.owner_revealed"], "true");
    assert_eq!(report.postconditions["credis.collateral_pledged"], "true");
    assert!(report.calldata.is_some());
    assert!(report.gas_totals[&GasLedger::UserTransaction] > 21_000);
    assert!(!report.storage.is_empty());
    assert!(!report.events.is_empty());
}
