//! Independent verifier for OCOMP PoC evidence bundles.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{ensure, Result, WrapErr};
use outbe_e2e_harness::ocomp_evidence::{
    assemble_lane, discover, manifest_in, missing_bundle_report, publish_report, require_pass,
    task_progress_report, verify_manifest, EvidenceMode, PlanningLedger,
};
#[cfg(feature = "ocomp-integration")]
use outbe_e2e_harness::{
    ocomp_capacity::{observe_capacity_host, OcompCapacityHostObservationV1},
    ocomp_evidence::assemble_capacity_evidence,
};
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::capacity::CapacityBudgetV1;

#[derive(Debug, Parser)]
#[command(name = "outbe-e2e-evidence")]
#[command(about = "Fail-closed OCOMP PoC planning-ledger and evidence verifier")]
struct Cli {
    /// Repository root.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Planning ledger, relative to the repository unless absolute.
    #[arg(long, default_value = "outbe-plan/off-chain-poc-evidence-ledger.yaml")]
    ledger: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Measure and publish exact OCM-26 host facts using Rust-owned probes.
    #[cfg(feature = "ocomp-integration")]
    CapacityHost {
        /// Real workspace filesystem used by all five cold runs.
        #[arg(long)]
        workspace: PathBuf,
        /// Immutable output JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Assemble exactly five public scenario records into CapacityEvidenceV1.
    #[cfg(feature = "ocomp-integration")]
    CapacityAssemble {
        /// Host observation emitted by `capacity-host`.
        #[arg(long)]
        host: PathBuf,
        /// Explicit per-dimension budget JSON.
        #[arg(long)]
        budget: PathBuf,
        /// Five distinct immutable public capacity scenario JSON files.
        #[arg(long = "scenario", required = true, num_args = 5)]
        scenarios: Vec<PathBuf>,
        /// Immutable output CapacityEvidenceV1 JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Parse and validate all ledger IDs, ownership and references.
    ValidateLedger,
    /// Print independently discovered and missing stable test IDs.
    Discover,
    /// Verify one published evidence bundle.
    Verify {
        /// Path to `run-manifest.json`.
        manifest: PathBuf,
        /// Optional directory for deterministic closure reports.
        #[arg(long)]
        report_dir: Option<PathBuf>,
    },
    /// Emit a narrow incremental claim after the named task-local tests pass.
    TaskProgress {
        /// `OCM-NN` task identity.
        task: String,
        /// Stable IDs proved by the immediately preceding task-local command.
        #[arg(long = "passed", required = true)]
        passed: Vec<String>,
    },
    /// Fail closed until every test in a lane has real evidence.
    Lane {
        /// Registered ledger lane such as `OCM-FAST`.
        lane: String,
        /// Directory containing the completed lane scenario records.
        #[arg(long)]
        evidence_dir: PathBuf,
    },
    /// Verify a complete bundle, or report every missing ID when none exists.
    Closure {
        /// Directory containing `run-manifest.json`.
        #[arg(long)]
        evidence_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = absolute(&cli.repo)?;
    let ledger_path = if cli.ledger.is_absolute() {
        cli.ledger.clone()
    } else {
        repo.join(&cli.ledger)
    };
    let ledger = PlanningLedger::parse(&ledger_path)?;

    match cli.command {
        #[cfg(feature = "ocomp-integration")]
        Command::CapacityHost { workspace, output } => {
            let workspace = resolve_from_current(&workspace)?;
            let observation = observe_capacity_host(&workspace)?;
            publish_json_output(&output, &observation)?;
            println!("{}", serde_json::to_string_pretty(&observation)?);
            Ok(())
        }
        #[cfg(feature = "ocomp-integration")]
        Command::CapacityAssemble {
            host,
            budget,
            scenarios,
            output,
        } => {
            let host: OcompCapacityHostObservationV1 = decode_json(&resolve_from_current(&host)?)?;
            let budget: CapacityBudgetV1 = decode_json(&resolve_from_current(&budget)?)?;
            let scenarios = scenarios
                .iter()
                .map(|path| resolve_from_current(path))
                .collect::<Result<Vec<_>>>()?;
            let (evidence, verified) = assemble_capacity_evidence(host, budget, &scenarios)?;
            publish_json_output(&output, &evidence)?;
            println!("{}", serde_json::to_string_pretty(&verified)?);
            Ok(())
        }
        Command::ValidateLedger => {
            println!(
                "ledger PASS: {} tests, {} lanes, {} tasks",
                ledger.tests.len(),
                ledger.lanes.len(),
                ledger.task_ownership.len()
            );
            Ok(())
        }
        Command::Discover => {
            let discovery = discover(&repo, &ledger)?;
            println!("{}", serde_json::to_string_pretty(&discovery)?);
            Ok(())
        }
        Command::Verify {
            manifest,
            report_dir,
        } => {
            let manifest = resolve_from_current(&manifest)?;
            let report = verify_manifest(&repo, &ledger, &manifest)?;
            if let Some(report_dir) = report_dir {
                publish_report(&report_dir, &report)?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
            require_pass(&report)
        }
        Command::TaskProgress { task, passed } => {
            let discovery = discover(&repo, &ledger)?;
            let report = task_progress_report(&ledger, &task, discovery, &passed)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            require_pass(&report)
        }
        Command::Lane { lane, evidence_dir } => {
            ensure!(ledger.lanes.contains_key(&lane), "unknown lane {lane}");
            let evidence_dir = resolve_from_current(&evidence_dir)?;
            let manifest = assemble_lane(&repo, &ledger, &lane, &evidence_dir)?;
            let report = verify_manifest(&repo, &ledger, &manifest)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            require_pass(&report)
        }
        Command::Closure { evidence_dir } => {
            let manifest = manifest_in(&evidence_dir);
            if !manifest.is_file() {
                let discovery = discover(&repo, &ledger)?;
                let report = missing_bundle_report(
                    &ledger,
                    EvidenceMode::PocClosure,
                    discovery,
                    format!("missing {}", manifest.display()),
                );
                println!("{}", serde_json::to_string_pretty(&report)?);
                return require_pass(&report);
            }
            let report = verify_manifest(&repo, &ledger, &manifest)?;
            publish_report(&evidence_dir, &report)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            require_pass(&report)
        }
    }
}

#[cfg(feature = "ocomp-integration")]
fn decode_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(
        &std::fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?,
    )
    .wrap_err_with(|| format!("decode typed JSON {}", path.display()))
}

#[cfg(feature = "ocomp-integration")]
fn publish_json_output<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let path = resolve_from_current(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("capacity output has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre::eyre!("capacity output file name is not UTF-8"))?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    outbe_e2e_harness::ocomp_evidence::publish_member(parent, name, &bytes)?;
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .wrap_err("resolve current directory")
        .map(|current| current.join(path))
}

fn resolve_from_current(path: &Path) -> Result<PathBuf> {
    absolute(path)
}
