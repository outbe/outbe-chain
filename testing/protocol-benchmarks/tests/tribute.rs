use outbe_protocol_benchmarks::{
    run_scenario, scenarios::tribute::TributeScenario, GasLedger, RunConfig, StorageOperationKind,
};

#[test]
fn tribute_non_zk_and_zk_creation_run_through_the_unified_interface() {
    for scenario in [TributeScenario::non_zk(), TributeScenario::zk()] {
        let report = run_scenario(
            &scenario,
            RunConfig {
                samples: 3,
                warmups: 0,
            },
        )
        .unwrap();

        assert_eq!(report.postconditions["tribute.created"], "true");
        assert!(report.gas_totals[&GasLedger::UserTransaction] > 3_000_000);
        assert_eq!(
            report
                .storage
                .iter()
                .filter(|entry| entry.operation == StorageOperationKind::Write)
                .map(|entry| entry.count)
                .sum::<u64>(),
            30
        );
        assert!(!report.events.is_empty());
        assert!(report
            .component_latency_ns
            .contains_key("enclave.process_offer"));
    }
}
