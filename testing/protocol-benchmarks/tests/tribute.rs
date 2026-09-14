use alloy_primitives::{Address, B256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_protocol_benchmarks::{
    run_scenario, scenarios::tribute::TributeScenario, GasLedger, RunConfig, ScenarioReport,
    StorageOperationKind,
};
use outbe_tribute::precompile::ITribute;
use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};

fn emitted(report: &ScenarioReport, emitter: Address, topic: B256) -> u64 {
    let (emitter, topic) = (format!("{emitter:#x}"), format!("{topic:#x}"));
    report
        .events
        .iter()
        .filter(|event| event.emitter == emitter && event.event == topic)
        .map(|event| event.count)
        .sum()
}

#[test]
fn zk_tribute_creation_runs_through_the_unified_interface() {
    let report = run_scenario(
        &TributeScenario,
        RunConfig {
            samples: 3,
            warmups: 0,
        },
    )
    .unwrap();

    assert_eq!(report.postconditions["tribute.created"], "true");

    // The in-process enclave step is the one latency the scenario cannot
    // derive from an outer timer, so it must be reported on its own.
    assert!(report
        .component_latency_ns
        .contains_key("enclave.process_offer"));

    // Issuance is ZK-only, so the proof-verification latencies of the one
    // measured path must be reported alongside it.
    assert!(report
        .component_latency_ns
        .contains_key("chain.ultrahonk_verify"));
    assert!(report
        .component_latency_ns
        .contains_key("chain.bn254_pair_reference"));

    // One issuance: the body projection and the ERC-721 issue event, both
    // emitted by the Tribute contract.
    assert_eq!(
        emitted(
            &report,
            TRIBUTE_ADDRESS,
            ITribute::TributeBodyStored::SIGNATURE_HASH
        ),
        1
    );
    assert_eq!(
        emitted(
            &report,
            TRIBUTE_ADDRESS,
            ITribute::TributeIssued::SIGNATURE_HASH
        ),
        1
    );

    // The aggregated storage trace is a second accounting of the same
    // operations as the storage gas components; both views must agree in
    // gas and in operation count, priced read=warm, write=reset.
    let trace_gas: u64 = report.storage.iter().map(|entry| entry.gas).sum();
    let trace_operations: u64 = report.storage.iter().map(|entry| entry.count).sum();
    let storage_components = report
        .gas_components
        .iter()
        .filter(|component| component.key.starts_with("storage."));
    let (component_gas, component_operations) = storage_components
        .fold((0_u64, 0_u64), |(gas, operations), component| {
            (gas + component.gas, operations + component.operations)
        });
    assert_eq!(trace_gas, component_gas);
    assert_eq!(trace_operations, component_operations);
    assert!(report.storage.iter().all(|entry| {
        let unit = match entry.operation {
            StorageOperationKind::Read => WARM_STORAGE_READ_COST,
            StorageOperationKind::Write => SSTORE_RESET,
        };
        entry.count > 0 && entry.gas == entry.count * unit
    }));
    assert!(
        report.storage.iter().any(|entry| {
            entry.module == "tribute" && entry.operation == StorageOperationKind::Write
        }),
        "issuing a Tribute must write Tribute state"
    );
    let reported_gas: u64 = report
        .gas_components
        .iter()
        .filter(|component| component.ledger == GasLedger::UserTransaction)
        .map(|component| component.gas)
        .sum();
    assert_eq!(report.gas_totals[&GasLedger::UserTransaction], reported_gas);
}
