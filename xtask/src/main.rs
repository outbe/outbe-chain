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
    /// Create a deterministic archive from an already verified signed bundle.
    Archive {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Build an immutable OCI image from an already verified signed bundle.
    Image {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        image: String,
        #[arg(long)]
        output: PathBuf,
        /// Push by digest and emit BuildKit SBOM/provenance attestations.
        #[arg(long)]
        push: bool,
    },
    /// Promote one exact ELF, signed SGX bundle and OCI image to a verified ReleaseManifest.
    Manifest {
        #[arg(long)]
        elf_manifest: PathBuf,
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        bundle_archive: PathBuf,
        #[arg(long)]
        oci_evidence: PathBuf,
        #[arg(long)]
        sbom: PathBuf,
        #[arg(long)]
        elf_evidence: PathBuf,
        #[arg(long)]
        sgx_evidence: PathBuf,
        #[arg(long)]
        hardware_evidence: PathBuf,
        #[arg(long)]
        output: PathBuf,
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
                SgxCommand::Archive { bundle, output } => {
                    sgx::archive(&repo_root, &bundle, &output)?;
                    println!(
                        "deterministic signed testnet SGX archive: {}",
                        output.display()
                    );
                }
                SgxCommand::Image {
                    bundle,
                    image,
                    output,
                    push,
                } => {
                    sgx::build_image(&repo_root, &bundle, &image, &output, push)?;
                    println!("testnet SGX OCI evidence: {}", output.display());
                }
                SgxCommand::Manifest {
                    elf_manifest,
                    bundle,
                    bundle_archive,
                    oci_evidence,
                    sbom,
                    elf_evidence,
                    sgx_evidence,
                    hardware_evidence,
                    output,
                } => {
                    sgx::finalize_release_manifest(
                        &repo_root,
                        &sgx::VerifiedReleaseInputs {
                            bundle,
                            bundle_archive,
                            elf_evidence,
                            elf_manifest,
                            hardware_evidence,
                            oci_evidence,
                            sbom,
                            sgx_evidence,
                        },
                        &output,
                    )?;
                    println!("verified testnet ReleaseManifest: {}", output.display());
                }
            },
        },
    }
    Ok(())
}
