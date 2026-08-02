//! Domain-neutral, fail-closed VerificationLedger index and bundle verifier.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{ensure, Result, WrapErr};
use outbe_e2e_harness::metadosis_evidence;
use outbe_e2e_harness::verification_ledger::{
    verify_evidence_set, verify_source_checkout, DomainEvidenceBundle, DomainEvidenceV1,
    VerificationLedger,
};

#[derive(Debug, Parser)]
#[command(name = "outbe-verification-ledger")]
#[command(about = "Verify versioned domain evidence against the repository ledger index")]
struct Cli {
    /// Repository root whose exact source/toolchain identity is being verified.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// VerificationLedger index, relative to the repository unless absolute.
    #[arg(long, default_value = "outbe-plan/verification-ledger.yaml")]
    index: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Strictly parse the index and every pinned domain pack.
    ValidateIndex,
    /// Verify one or more domain manifests and every referenced member byte.
    Verify {
        /// DomainEvidenceV1 JSON files. Repeat once per domain.
        #[arg(long = "evidence", required = true)]
        evidence: Vec<PathBuf>,
        /// Bundle roots paired positionally with `--evidence`.
        #[arg(long = "bundle-root", required = true)]
        bundle_roots: Vec<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = absolute(&cli.repo)?;
    let index = resolve(&repo, &cli.index);
    let ledger = VerificationLedger::parse(&index)?;

    match cli.command {
        Command::ValidateIndex => {
            println!(
                "verification ledger PASS: {} domain packs",
                ledger.domain_packs.len()
            );
            Ok(())
        }
        Command::Verify {
            evidence,
            bundle_roots,
        } => {
            ensure!(
                evidence.len() == bundle_roots.len(),
                "--evidence and --bundle-root counts differ"
            );
            let evidence_paths = evidence
                .iter()
                .map(|path| resolve(&repo, path))
                .collect::<Vec<_>>();
            let evidence = evidence_paths
                .iter()
                .map(|path| decode_evidence(path))
                .collect::<Result<Vec<_>>>()?;
            let bundle_roots = bundle_roots
                .iter()
                .map(|path| resolve(&repo, path))
                .collect::<Vec<_>>();

            for ((domain, evidence_path), bundle_root) in
                evidence.iter().zip(&evidence_paths).zip(&bundle_roots)
            {
                if domain.namespace == metadosis_evidence::METADOSIS_NAMESPACE {
                    // Generic closure validates identity, catalog shape and
                    // member bytes. Metadosis additionally derives every
                    // status from its retained runner receipt; invoke that
                    // domain adapter before accepting the cross-pack set.
                    metadosis_evidence::verify(&repo, &index, bundle_root, evidence_path)?;
                }
            }
            for domain in &evidence {
                verify_source_checkout(&repo, &domain.source)?;
            }
            let bundles = evidence
                .iter()
                .zip(&bundle_roots)
                .map(|(domain, root)| DomainEvidenceBundle::new(domain, root))
                .collect::<Vec<_>>();
            verify_evidence_set(&ledger, &bundles)?;
            println!(
                "verification ledger PASS: {} domains at {}",
                evidence.len(),
                evidence[0].source.sha
            );
            Ok(())
        }
    }
}

fn decode_evidence(path: &Path) -> Result<DomainEvidenceV1> {
    serde_json::from_slice(
        &std::fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?,
    )
    .wrap_err_with(|| format!("decode DomainEvidenceV1 {}", path.display()))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .wrap_err("resolve current directory")
        .map(|current| current.join(path))
}

fn resolve(repo: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo.join(path)
    }
}
