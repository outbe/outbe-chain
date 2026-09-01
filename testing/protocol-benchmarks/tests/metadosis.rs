use outbe_protocol_benchmarks::{
    run_scenario, scenarios::metadosis::MetadosisScenario, GasLedger, RunConfig,
};

#[test]
fn metadosis_worldwide_day_creation_is_typed_and_metered() {
    let report = run_scenario(
        &MetadosisScenario::worldwide_day(),
        RunConfig {
            samples: 3,
            warmups: 0,
        },
    )
    .unwrap();

    assert_eq!(report.postconditions["metadosis.wwd_created"], "true");
    assert_eq!(report.postconditions["metadosis.wwd_active"], "true");
    assert!(report.gas_totals[&GasLedger::SystemInternal] > 0);
    assert!(!report.storage.is_empty());
}
