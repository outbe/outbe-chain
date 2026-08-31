use outbe_protocol_benchmarks::{
    check_gas_baseline, update_gas_baseline, ExecutionClass, GasBaseline, GasComponent, GasLedger,
    LatencyStats, Profile, ScenarioMetadata, ScenarioReport, REPORT_SCHEMA,
};
use std::collections::BTreeMap;

fn report(latency: u64) -> ScenarioReport {
    ScenarioReport {
        schema: REPORT_SCHEMA.to_owned(),
        metadata: ScenarioMetadata::new(
            "test/snapshot",
            "Snapshot",
            ExecutionClass::UserTransaction,
            Profile::Single,
        ),
        samples: 3,
        setup_latency_ns: latency * 100,
        setup_components_ns: BTreeMap::from([("fixture".to_owned(), latency * 100)]),
        total_latency_ns: LatencyStats {
            min: latency,
            median: latency,
            p95: latency,
            max: latency,
        },
        component_latency_ns: BTreeMap::from([(
            "runtime".to_owned(),
            LatencyStats {
                min: latency,
                median: latency,
                p95: latency,
                max: latency,
            },
        )]),
        calldata: None,
        gas_totals: BTreeMap::from([(GasLedger::UserTransaction, 100)]),
        gas_components: vec![GasComponent::new(
            GasLedger::UserTransaction,
            "runtime",
            100,
            1,
        )],
        storage: Vec::new(),
        events: Vec::new(),
        child_frames: Vec::new(),
        postconditions: BTreeMap::from([("created".to_owned(), "true".to_owned())]),
        artifacts: BTreeMap::from([("fixture".to_owned(), "sha256:01".to_owned())]),
    }
}

#[test]
fn gas_baseline_excludes_latency_and_environment_measurements() {
    let fast = GasBaseline::from_reports(1, &[report(10)]).unwrap();
    let slow = GasBaseline::from_reports(1, &[report(10_000)]).unwrap();

    assert_eq!(fast, slow);
    let json = serde_json::to_string_pretty(&fast).unwrap();
    assert!(!json.contains("latency"));
    assert!(!json.contains("samples"));
    assert!(json.contains("sha256:01"));
}

#[test]
fn baseline_update_is_atomic_and_check_detects_drift() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gas-v1.json");
    let baseline = GasBaseline::from_reports(1, &[report(10)]).unwrap();
    update_gas_baseline(&path, &baseline).unwrap();
    check_gas_baseline(&path, &baseline).unwrap();

    let mut changed = baseline.clone();
    *changed
        .scenarios
        .get_mut("test/snapshot")
        .unwrap()
        .gas_totals
        .get_mut(&GasLedger::UserTransaction)
        .unwrap() += 1;
    let error = check_gas_baseline(&path, &changed).unwrap_err();
    assert!(error.to_string().contains("gas baseline differs"));
    check_gas_baseline(&path, &baseline).unwrap();
}
