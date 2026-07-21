use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;
use xtask::release::sgx;

#[derive(Debug, Parser)]
#[command(about = "Outbe repository development and release automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build, verify and publish release artifacts.
    Release(ReleaseArgs),
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Prepare, authorize and verify a pre-signed Gramine SGX bundle.
    Sgx(SgxArgs),
}

#[derive(Debug, Args)]
struct SgxArgs {
    #[command(subcommand)]
    command: SgxCommand,
}

#[derive(Debug, Subcommand)]
enum SgxCommand {
    /// Prepare an unsigned deterministic Gramine bundle from a verified ELF build.
    Prepare {
        #[arg(long)]
        elf_output: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare two independently prepared unsigned bundles.
    Compare {
        #[arg(long)]
        first: PathBuf,
        #[arg(long)]
        second: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Authorize an unsigned bundle with the protected testnet SGX key.
    Sign {
        #[arg(long)]
        unsigned: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify checksums, canonical metadata and the enclave SIGSTRUCT.
    Verify {
        #[arg(long)]
        bundle: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = sgx::repository_root()?;
    match cli.command {
        Command::Release(release) => match release.command {
            ReleaseCommand::Sgx(sgx_args) => match sgx_args.command {
                SgxCommand::Prepare { elf_output, output } => {
                    sgx::prepare(&repo_root, &elf_output, &output)?;
                    println!(
                        "unsigned deterministic testnet SGX bundle: {}",
                        output.display()
                    );
                }
                SgxCommand::Compare {
                    first,
                    second,
                    output,
                } => {
                    sgx::compare(&first, &second, &output)?;
                    println!(
                        "unsigned SGX reproducibility evidence: {}",
                        output.display()
                    );
                }
                SgxCommand::Sign {
                    unsigned,
                    key_file,
                    output,
                } => {
                    sgx::sign(&repo_root, &unsigned, &key_file, &output)?;
                    println!("signed testnet SGX bundle: {}", output.display());
                }
                SgxCommand::Verify { bundle } => {
                    sgx::verify(&repo_root, &bundle)?;
                    println!("verified signed testnet SGX bundle: {}", bundle.display());
                }
            },
        },
    }
    Ok(())
}
