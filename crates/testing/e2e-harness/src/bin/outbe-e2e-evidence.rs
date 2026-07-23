//! Independent verifier for OCOMP PoC evidence bundles.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{ensure, Result, WrapErr};
use outbe_e2e_harness::ocomp_evidence::{
    discover, manifest_in, missing_bundle_report, publish_report, require_pass,
    task_progress_report, verify_manifest, EvidenceMode, PlanningLedger,
};

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
        Command::Lane { lane } => {
            ensure!(ledger.lanes.contains_key(&lane), "unknown lane {lane}");
            let discovery = discover(&repo, &ledger)?;
            let lane_ids = ledger.lane_test_ids(&lane);
            let discovered = discovery
                .discovered
                .iter()
                .filter(|test| lane_ids.contains(*test))
                .cloned()
                .collect::<Vec<_>>();
            let missing = lane_ids
                .iter()
                .filter(|test| !discovered.contains(test))
                .cloned()
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "mode": "lane_placeholder",
                    "lane": lane,
                    "status": "MISSING",
                    "discovered_test_ids": discovered,
                    "missing_test_ids": missing,
                    "reason": "OCM-00 exposes the lane but no run manifest can prove it yet"
                }))?
            );
            eyre::bail!("lane {lane} has no verified exact-artifact evidence")
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
