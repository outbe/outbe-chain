use outbe_protocol_benchmarks::{
    run_scenario, scenarios::credis_boundary::CredisVaultBoundaryScenario, ChildFrameFidelity,
    ExecutionLayer, RunConfig, ScenarioFidelity,
};

#[test]
fn rust_vault_boundary_is_explicitly_partial_and_never_claims_smart_account_gas() {
    let report = run_scenario(
        &CredisVaultBoundaryScenario,
        RunConfig {
            samples: 3,
            warmups: 0,
        },
    )
    .unwrap();

    assert_eq!(report.metadata.execution_layer, ExecutionLayer::Marginal);
    assert_eq!(report.metadata.fidelity, ScenarioFidelity::PartialStubbed);
    assert_eq!(report.child_frames.len(), 5);
    assert!(report
        .child_frames
        .iter()
        .all(|frame| frame.fidelity == ChildFrameFidelity::BenchmarkStub));
    assert_eq!(
        report.postconditions["credis.partial_result_is_production_total"],
        "false"
    );
    assert!(report.postconditions["credis.full_smart_account_gas"].contains("TODO"));
}
