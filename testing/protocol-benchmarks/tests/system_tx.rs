use std::collections::BTreeSet;

use outbe_primitives::system_tx::SystemTxKind;
use outbe_protocol_benchmarks::{
    run_scenario, scenarios::system_tx::SystemTxScenario, ChildFrameFidelity, GasLedger, RunConfig,
    ScenarioFidelity,
};

#[test]
fn every_system_tx_kind_has_an_explicit_rust_only_scenario() {
    let scenarios = SystemTxScenario::all();
    assert_eq!(scenarios.len(), 10);

    let kinds = scenarios
        .iter()
        .map(SystemTxScenario::kind)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([
            SystemTxKind::CertifiedParentAccounting,
            SystemTxKind::LateFinalizeCredits,
            SystemTxKind::OcompLifecycleBegin,
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::BoundaryOutcome,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
            SystemTxKind::OcompTerminalRequest,
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
        .unwrap_or_else(|error| panic!("{:?} failed: {error}", scenario.kind()));

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
