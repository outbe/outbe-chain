use outbe_protocol_benchmarks::{
    run_scenario, BenchmarkScenario, ExecutionClass, GasComponent, GasLedger, Observation, Profile,
    RunConfig, ScenarioMetadata,
};
use std::cell::Cell;

struct StableScenario;

impl BenchmarkScenario for StableScenario {
    type Prepared = ();

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "test/stable",
            "Stable scenario",
            ExecutionClass::UserTransaction,
            Profile::Single,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        Ok(())
    }

    fn run_once(&self, _prepared: &Self::Prepared) -> Result<Observation, String> {
        Ok(Observation::new(
            [(GasLedger::UserTransaction, 21_100)],
            [
                GasComponent::new(GasLedger::UserTransaction, "transaction.base", 21_000, 1),
                GasComponent::new(GasLedger::UserTransaction, "storage.read", 100, 1),
            ],
        )
        .with_postcondition("completed", "true"))
    }
}

struct VariableLatencyScenario {
    iteration: Cell<u64>,
}

impl BenchmarkScenario for VariableLatencyScenario {
    type Prepared = ();

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "test/variable-latency",
            "Variable latency",
            ExecutionClass::InternalTransition,
            Profile::Single,
        )
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        Ok(())
    }

    fn run_once(&self, _prepared: &Self::Prepared) -> Result<Observation, String> {
        let iteration = self.iteration.get() + 1;
        self.iteration.set(iteration);
        Ok(Observation::new(
            [(GasLedger::UserTransaction, 100)],
            [GasComponent::new(
                GasLedger::UserTransaction,
                "runtime",
                100,
                1,
            )],
        )
        .with_latency("runtime", iteration * 10)
        .with_postcondition("completed", "true"))
    }
}

#[test]
fn runner_reports_stable_gas_and_measured_samples() {
    let report = run_scenario(
        &StableScenario,
        RunConfig {
            samples: 3,
            warmups: 1,
        },
    )
    .unwrap();

    assert_eq!(report.metadata.id, "test/stable");
    assert_eq!(report.samples, 3);
    assert_eq!(report.gas_totals[&GasLedger::UserTransaction], 21_100);
    assert_eq!(report.gas_components.len(), 2);
    assert!(report.total_latency_ns.min <= report.total_latency_ns.max);
}

#[test]
fn variable_latency_is_aggregated_without_causing_gas_drift() {
    let scenario = VariableLatencyScenario {
        iteration: Cell::new(0),
    };
    let report = run_scenario(
        &scenario,
        RunConfig {
            samples: 3,
            warmups: 1,
        },
    )
    .unwrap();

    let runtime = report.component_latency_ns["runtime"];
    assert_eq!(runtime.min, 20);
    assert_eq!(runtime.median, 30);
    assert_eq!(runtime.p95, 40);
    assert_eq!(runtime.max, 40);
}
