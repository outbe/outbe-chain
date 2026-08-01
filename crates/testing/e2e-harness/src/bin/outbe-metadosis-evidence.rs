//! Metadosis Citadel Linux evidence runner, assembler, and verifier.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr};
use outbe_e2e_harness::{metadosis_evidence, metadosis_process};

#[derive(Debug, Parser)]
#[command(name = "outbe-metadosis-evidence")]
#[command(about = "Run and verify exact-revision Metadosis Citadel evidence")]
struct Cli {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "outbe-plan/verification-ledger.yaml")]
    index: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the six-case P0 debug/release environment-parity lane.
    P0Parity {
        #[arg(long)]
        output: PathBuf,
    },
    /// Run the four-validator fresh-devnet lifecycle lane.
    FreshDevnet {
        #[arg(long)]
        output: PathBuf,
    },
    /// Execute every checked-in Metadosis test exactly once and retain raw facts.
    Run {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        linux_image_digest: String,
        #[arg(long)]
        launcher_nonce: String,
    },
    /// Run the complete evidence lane in one exact Linux/amd64 Docker image.
    ContainerRun {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        image: String,
    },
    /// Derive DomainEvidenceV1; no status or test result is accepted as input.
    Assemble {
        #[arg(long)]
        bundle_root: PathBuf,
        #[arg(long, default_value = "runner-receipt.json")]
        receipt: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Reconstruct the manifest and run generic plus Metadosis policy checks.
    Verify {
        #[arg(long)]
        bundle_root: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
    },
    /// Assemble and verify a completed container run in the pinned Linux lane.
    Finalize {
        #[arg(long)]
        bundle_root: PathBuf,
        #[arg(long, default_value = "domain-evidence.json")]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = absolute(&cli.repo)?;
    let index = resolve(&repo, &cli.index);
    match cli.command {
        Command::P0Parity { output } => {
            metadosis_process::run_p0_parity(&repo, &absolute(&output)?)?;
            println!("Metadosis P0 parity evidence: {}", output.display());
        }
        Command::FreshDevnet { output } => {
            metadosis_process::run_fresh_devnet(&repo, &absolute(&output)?)?;
            println!("Metadosis fresh-devnet evidence: {}", output.display());
        }
        Command::Run {
            output,
            linux_image_digest,
            launcher_nonce,
        } => {
            let receipt = metadosis_evidence::run(
                &repo,
                &index,
                &absolute(&output)?,
                &linux_image_digest,
                &launcher_nonce,
            )?;
            println!("Metadosis runner receipt: {}", receipt.display());
        }
        Command::ContainerRun { output, image } => {
            let evidence = metadosis_evidence::run_in_pinned_linux_container(
                &repo,
                &index,
                &absolute(&output)?,
                &image,
            )?;
            println!("Metadosis container evidence PASS: {}", evidence.display());
        }
        Command::Assemble {
            bundle_root,
            receipt,
            output,
        } => {
            let bundle_root = absolute(&bundle_root)?;
            let receipt = resolve(&bundle_root, &receipt);
            let output = resolve(&bundle_root, &output);
            let evidence =
                metadosis_evidence::assemble(&repo, &index, &bundle_root, &receipt, &output)?;
            println!(
                "Metadosis evidence assembled: {} tests at {}",
                evidence.tests.len(),
                output.display()
            );
        }
        Command::Verify {
            bundle_root,
            evidence,
        } => {
            let bundle_root = absolute(&bundle_root)?;
            let evidence = resolve(&bundle_root, &evidence);
            metadosis_evidence::verify(&repo, &index, &bundle_root, &evidence)?;
            println!("Metadosis evidence PASS: {}", evidence.display());
        }
        Command::Finalize {
            bundle_root,
            output,
        } => {
            let bundle_root = absolute(&bundle_root)?;
            let receipt = bundle_root.join("runner-receipt.json");
            let output = resolve(&bundle_root, &output);
            metadosis_evidence::assemble(&repo, &index, &bundle_root, &receipt, &output)?;
            metadosis_evidence::verify(&repo, &index, &bundle_root, &output)?;
            println!("Metadosis evidence PASS: {}", output.display());
        }
    }
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

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}
