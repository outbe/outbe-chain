use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    BenchmarkError, ChildFrameFidelity, GasBaseline, GasLedger, LatencyStats, ScenarioFidelity,
    ScenarioReport, StorageOperationKind, REPORT_SCHEMA,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkEnvironment {
    pub cpu: String,
    pub process_rss_mib: Option<f64>,
}

impl BenchmarkEnvironment {
    #[must_use]
    pub fn capture() -> Self {
        Self {
            cpu: cpu_model(),
            process_rss_mib: process_rss_mib(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkRunReport {
    pub schema: String,
    pub environment: BenchmarkEnvironment,
    pub scenarios: Vec<ScenarioReport>,
}

impl BenchmarkRunReport {
    #[must_use]
    pub fn new(scenarios: Vec<ScenarioReport>) -> Self {
        Self {
            schema: REPORT_SCHEMA.to_owned(),
            environment: BenchmarkEnvironment::capture(),
            scenarios,
        }
    }
}

pub fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), BenchmarkError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| BenchmarkError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, value).map_err(|error| BenchmarkError::Json {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        writer
            .write_all(b"\n")
            .map_err(|error| BenchmarkError::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            })?;
        writer.flush().map_err(|error| BenchmarkError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| BenchmarkError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
    temporary
        .persist(path)
        .map_err(|error| BenchmarkError::Io {
            path: path.display().to_string(),
            detail: error.error.to_string(),
        })?;
    Ok(())
}

pub fn check_gas_baseline(path: &Path, actual: &GasBaseline) -> Result<(), BenchmarkError> {
    let expected = read_gas_baseline(path)?;
    if &expected == actual {
        Ok(())
    } else {
        Err(BenchmarkError::BaselineMismatch {
            path: path.display().to_string(),
        })
    }
}

pub fn update_gas_baseline(path: &Path, baseline: &GasBaseline) -> Result<(), BenchmarkError> {
    write_json_atomic(path, baseline)
}

fn read_gas_baseline(path: &Path) -> Result<GasBaseline, BenchmarkError> {
    let file = File::open(path).map_err(|error| BenchmarkError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    serde_json::from_reader(BufReader::new(file)).map_err(|error| BenchmarkError::Json {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

#[must_use]
pub fn render_text(report: &BenchmarkRunReport) -> String {
    let mut output = String::new();
    output.push_str("OUTBE PROTOCOL CREATION GAS + LATENCY BENCHMARK\n");
    output.push_str("=================================================\n");
    output.push_str(&format!("Schema:              {}\n", report.schema));
    output.push_str(&format!(
        "CPU:                 {}\n",
        report.environment.cpu
    ));
    match report.environment.process_rss_mib {
        Some(rss) => output.push_str(&format!("Process RSS:         {rss:.1} MiB\n")),
        None => output.push_str("Process RSS:         unknown\n"),
    }
    output.push_str(&format!(
        "Scenarios:           {}\n",
        report.scenarios.len()
    ));

    for scenario in &report.scenarios {
        render_scenario(&mut output, scenario);
    }
    output.push_str("\nNOTES\n");
    output
        .push_str("- Setup/deployment/fixture work is excluded from sampled operation latency.\n");
    output.push_str("- Gas snapshots exclude CPU, RSS, setup latency, and operation latency.\n");
    output
        .push_str("- Rust marginal, SystemVisible, and SystemInternal ledgers are never summed.\n");
    output.push_str(
        "- SGX transitions, IPC, attestation, networking, and block production are excluded.\n",
    );
    output
}

fn render_scenario(output: &mut String, report: &ScenarioReport) {
    output.push_str("\n-------------------------------------------------\n");
    output.push_str(&format!("{}\n", report.metadata.display_name));
    output.push_str(&format!("id:                  {}\n", report.metadata.id));
    let fidelity = match report.metadata.fidelity {
        ScenarioFidelity::Full => "FULL",
        ScenarioFidelity::PartialStubbed => "PARTIAL / STUBBED",
    };
    output.push_str(&format!("fidelity:            {fidelity}\n"));
    output.push_str(&format!("samples:             {}\n", report.samples));
    output.push_str(&format!(
        "setup total:         {:.3} ms\n",
        ns_to_ms(report.setup_latency_ns)
    ));
    for (component, latency) in &report.setup_components_ns {
        output.push_str(&format!(
            "setup/{component:<32} {:>12.3} ms\n",
            ns_to_ms(*latency)
        ));
    }

    output.push_str("\nLATENCY (milliseconds; setup excluded)\n");
    output.push_str(&format!(
        "{:<38} {:>12} {:>12} {:>12} {:>12}\n",
        "component", "min", "median", "p95", "max"
    ));
    latency_row(output, "operation.total", report.total_latency_ns);
    for (component, stats) in &report.component_latency_ns {
        latency_row(output, component, *stats);
    }

    if let Some(calldata) = report.calldata {
        output.push_str("\nCALLDATA\n");
        output.push_str(&format!("bytes:               {}\n", calldata.bytes));
        output.push_str(&format!("zero bytes:          {}\n", calldata.zero_bytes));
        output.push_str(&format!(
            "non-zero bytes:      {}\n",
            calldata.nonzero_bytes
        ));
        output.push_str(&format!(
            "intrinsic gas:       {}\n",
            calldata.intrinsic_gas()
        ));
    }

    output.push_str("\nGAS BREAKDOWN\n");
    output.push_str(&format!(
        "{:<46} {:>16} {:>12}\n",
        "component", "gas", "operations"
    ));
    for component in &report.gas_components {
        output.push_str(&format!(
            "{:<46} {:>16} {:>12}\n",
            component.key, component.gas, component.operations
        ));
    }
    for (ledger, total) in &report.gas_totals {
        output.push_str(&format!("TOTAL {ledger:?}: {total}\n"));
    }

    let mut storage_by_module = BTreeMap::<(String, StorageOperationKind), (u64, u64)>::new();
    for entry in &report.storage {
        let totals = storage_by_module
            .entry((entry.module.clone(), entry.operation))
            .or_default();
        totals.0 = totals.0.saturating_add(entry.count);
        totals.1 = totals.1.saturating_add(entry.gas);
    }
    output.push_str("\nSTORAGE BY MODULE\n");
    output.push_str(&format!(
        "{:<28} {:<8} {:>12} {:>16}\n",
        "module", "kind", "operations", "gas"
    ));
    for ((module, operation), (count, gas)) in storage_by_module {
        let operation = format!("{operation:?}");
        output.push_str(&format!(
            "{module:<28} {operation:<8} {count:>12} {gas:>16}\n"
        ));
    }
    output.push_str("\nSTORAGE SLOTS\n");
    output.push_str(&format!(
        "{:<22} {:<42} {:<66} {:<8} {:>8} {:>12}\n",
        "module", "address", "slot", "kind", "count", "gas"
    ));
    for entry in &report.storage {
        output.push_str(&format!(
            "{:<22} {:<42} {:<66} {:?} {:>8} {:>12}\n",
            entry.module, entry.address, entry.slot, entry.operation, entry.count, entry.gas
        ));
    }

    output.push_str("\nEVENTS\n");
    for event in &report.events {
        output.push_str(&format!(
            "{} {} x{}\n",
            event.emitter, event.event, event.count
        ));
    }

    if !report.child_frames.is_empty() {
        output.push_str("\nCHILD FRAMES\n");
        output.push_str(&format!(
            "{:<34} {:<16} {:<12} {:>12}\n",
            "label", "fidelity", "status", "gas"
        ));
        let mut production_gas = 0_u64;
        let mut stub_gas = 0_u64;
        for frame in &report.child_frames {
            match frame.fidelity {
                ChildFrameFidelity::Production => {
                    production_gas = production_gas.saturating_add(frame.gas_used);
                }
                ChildFrameFidelity::BenchmarkStub => {
                    stub_gas = stub_gas.saturating_add(frame.gas_used);
                }
            }
            output.push_str(&format!(
                "{:<34} {:?} {:<12} {:>12}\n",
                frame.label, frame.fidelity, frame.status, frame.gas_used
            ));
        }
        output.push_str(&format!(
            "production frame gas subtotal: {production_gas}\n"
        ));
        output.push_str(&format!("stub frame gas subtotal:       {stub_gas}\n"));
    }

    if !report.postconditions.is_empty() {
        output.push_str("\nPOSTCONDITIONS\n");
        for (key, evidence) in &report.postconditions {
            output.push_str(&format!("{key}: {evidence}\n"));
        }
    }

    if report.metadata.execution_class == crate::ExecutionClass::SystemTransaction {
        output.push_str("\nSYSTEM GAS LEDGERS (reported separately)\n");
        for ledger in [GasLedger::SystemVisible, GasLedger::SystemInternal] {
            if let Some(total) = report.gas_totals.get(&ledger) {
                output.push_str(&format!("{ledger:?}: {total}\n"));
            }
        }
    }
}

fn latency_row(output: &mut String, label: &str, value: LatencyStats) {
    output.push_str(&format!(
        "{label:<38} {:>12.6} {:>12.6} {:>12.6} {:>12.6}\n",
        ns_to_ms(value.min),
        ns_to_ms(value.median),
        ns_to_ms(value.p95),
        ns_to_ms(value.max),
    ));
}

fn ns_to_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|tail| tail.split_once(':'))
                    .map(|(_, value)| value.trim().to_owned())
            })
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn process_rss_mib() -> Option<f64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|kib| kib / 1_024.0)
}
