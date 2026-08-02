//! Independent verifier for OCOMP PoC evidence bundles.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{ensure, Result, WrapErr};
use outbe_e2e_harness::metadosis_p0::{
    canonical_final_fixture_sha256, capture_outbe_chain_feature_tree, current_clean_git_revision,
    verify_metadosis_p0_parity, MetadosisP0Case, MetadosisP0CaseInput,
};
use outbe_e2e_harness::ocomp_evidence::{
    assemble_closure, assemble_lane, closure_manifest_in, discover, manifest_in, publish_report,
    require_pass, task_progress_report, verify_manifest, verify_retained_semantics, PlanningLedger,
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
    /// Verify the six-run Metadosis P0 env-independence evidence lane.
    MetadosisP0Parity {
        /// Normal debug `outbe-chain` artifact used by the debug cases.
        #[arg(long)]
        debug_node: PathBuf,
        /// Normal release `outbe-chain` artifact used by the release cases.
        #[arg(long)]
        release_node: PathBuf,
        /// `cargo tree -e features -i outbe-metadosis` output for the node.
        #[arg(long)]
        feature_tree: PathBuf,
        /// Debug scenario evidence with the removed env input unset.
        #[arg(long)]
        debug_unset: PathBuf,
        /// Debug scenario evidence with the removed env input set to its old value.
        #[arg(long)]
        debug_named: PathBuf,
        /// Debug scenario evidence with the removed env input set arbitrarily.
        #[arg(long)]
        debug_arbitrary: PathBuf,
        /// Release scenario evidence with the removed env input unset.
        #[arg(long)]
        release_unset: PathBuf,
        /// Release scenario evidence with the removed env input set to its old value.
        #[arg(long)]
        release_named: PathBuf,
        /// Release scenario evidence with the removed env input set arbitrarily.
        #[arg(long)]
        release_arbitrary: PathBuf,
        /// Immutable parity evidence JSON.
        #[arg(long)]
        output: PathBuf,
    },
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
        Command::MetadosisP0Parity {
            debug_node,
            release_node,
            feature_tree,
            debug_unset,
            debug_named,
            debug_arbitrary,
            release_unset,
            release_named,
            release_arbitrary,
            output,
        } => {
            let debug_node = resolve_from_current(&debug_node)?;
            let release_node = resolve_from_current(&release_node)?;
            let feature_tree = resolve_from_current(&feature_tree)?;
            let retained_feature_tree = std::fs::read(&feature_tree)
                .wrap_err_with(|| format!("read {}", feature_tree.display()))?;
            let exact_feature_tree = capture_outbe_chain_feature_tree(&repo)?;
            ensure!(
                retained_feature_tree == exact_feature_tree,
                "retained feature tree differs from the exact verifier command"
            );
            let source_revision = current_clean_git_revision(&repo)?;
            let final_fixture_sha256 = canonical_final_fixture_sha256(&repo)?;
            let cases = [
                (MetadosisP0Case::DebugUnset, debug_unset),
                (MetadosisP0Case::DebugNamed, debug_named),
                (MetadosisP0Case::DebugArbitrary, debug_arbitrary),
                (MetadosisP0Case::ReleaseUnset, release_unset),
                (MetadosisP0Case::ReleaseNamed, release_named),
                (MetadosisP0Case::ReleaseArbitrary, release_arbitrary),
            ]
            .into_iter()
            .map(|(case, evidence_dir)| {
                Ok(MetadosisP0CaseInput {
                    case,
                    evidence_dir: resolve_from_current(&evidence_dir)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
            let evidence = verify_metadosis_p0_parity(
                &source_revision,
                &exact_feature_tree,
                &debug_node,
                &release_node,
                &final_fixture_sha256,
                &cases,
            )?;
            publish_json_output(&output, &evidence)?;
            println!("{}", serde_json::to_string_pretty(&evidence)?);
            Ok(())
        }
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
            verify_retained_semantics(&repo, &ledger, &manifest)?;
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
            verify_retained_semantics(&repo, &ledger, &manifest)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            require_pass(&report)
        }
        Command::Closure { evidence_dir } => {
            let evidence_dir = resolve_from_current(&evidence_dir)?;
            let direct_manifest = manifest_in(&evidence_dir);
            let manifest = if direct_manifest.is_file() {
                direct_manifest
            } else {
                let closure_manifest = closure_manifest_in(&evidence_dir);
                if closure_manifest.is_file() {
                    closure_manifest
                } else {
                    assemble_closure(&repo, &ledger, &evidence_dir)?
                }
            };
            let report = verify_manifest(&repo, &ledger, &manifest)?;
            verify_retained_semantics(&repo, &ledger, &manifest)?;
            let report_dir = manifest
                .parent()
                .ok_or_else(|| eyre::eyre!("closure manifest has no parent directory"))?;
            publish_report(report_dir, &report)?;
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
