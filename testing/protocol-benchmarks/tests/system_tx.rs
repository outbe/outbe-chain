use std::collections::BTreeSet;

use outbe_protocol_benchmarks::{
    run_scenario, scenarios::system_tx::SystemTxScenario, ChildFrameFidelity, GasLedger, RunConfig,
    ScenarioFidelity,
};

#[test]
fn every_system_tx_kind_has_an_explicit_rust_only_scenario() {
    let scenarios = SystemTxScenario::all();
    assert_eq!(scenarios.len(), 8);

    let kinds = scenarios
        .iter()
        .map(SystemTxScenario::name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([
            "certified-parent-accounting",
            "late-finalize-credits",
            "cycle-tick",
            "boundary-outcome",
            "tee-bootstrap",
            "oracle-slash-window",
            "hook-events",
            "radicle-publication",
        ])
    );
}

#[test]
fn rust_only_system_tx_reports_visible_and_internal_ledgers_separately() {
    for scenario in SystemTxScenario::all() {
        let report = run_scenario(
            &scenario,
            RunConfig {
                samples: 3,
                warmups: 0,
            },
        )
        .unwrap_or_else(|error| panic!("{} failed: {error}", scenario.name()));

        assert_eq!(
            report.metadata.id,
            format!("system-tx/{}/rust-wire/single", scenario.name())
        );
        assert_eq!(
            report.artifacts["visible.block_gas_limit"], "32000000",
            "all block heights use the uniform 32M block gas limit"
        );
        assert_eq!(report.metadata.fidelity, ScenarioFidelity::PartialStubbed);
        assert!(report.calldata.is_some());
        assert!(report.gas_totals[&GasLedger::SystemVisible] >= 21_000);
        assert_eq!(report.gas_totals[&GasLedger::SystemInternal], 0);
        assert_eq!(report.child_frames.len(), 1);
        assert_eq!(
            report.child_frames[0].fidelity,
            ChildFrameFidelity::BenchmarkStub
        );
        assert_eq!(report.child_frames[0].gas_used, 0);
        assert_eq!(
            report
                .postconditions
                .get("system_tx.codec_round_trip_kind")
                .map(String::as_str),
            Some("true")
        );
        assert!(report
            .postconditions
            .get("system_tx.full_execution_gas")
            .is_some_and(|evidence| evidence.contains("outbe-chain-6le.7")));
    }
}
