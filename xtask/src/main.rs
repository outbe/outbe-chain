use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;
use xtask::{ocomp, release::sgx, stablecoin};

#[derive(Debug, Parser)]
#[command(about = "Outbe repository development and release automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build, verify and publish release artifacts.
    Release(Box<ReleaseArgs>),
    /// Validate Stablecoin V1 repository and genesis invariants.
    Stablecoin(StablecoinArgs),
    /// Generate and verify Off-chain Computation PoC development artifacts.
    Ocomp(OcompArgs),
}

#[derive(Debug, Args)]
struct OcompArgs {
    #[command(subcommand)]
    command: OcompCommand,
}

#[derive(Debug, Subcommand)]
enum OcompCommand {
    /// Emit the frozen OCM-26 budget derived from Rust authorities.
    CapacityBudget {
        /// Destination for the deterministic typed budget JSON.
        #[arg(long)]
        output: PathBuf,
    },
    /// Execute five immutable systemd/cgroup cold runs and generate capacity.
    CapacityMeasure {
        /// New directory that will retain all five cold namespaces and evidence.
        #[arg(long)]
        output_dir: PathBuf,
        /// Exact generated-limits manifest bound into the capacity profile.
        #[arg(long)]
        limits_manifest: PathBuf,
        /// Destination for the deterministic generated capacity manifest.
        #[arg(long)]
        generated_capacity: PathBuf,
    },
    /// Verify five cold OCOMP runs and emit one deterministic capacity profile.
    Capacity {
        /// Typed `CapacityEvidenceV1` JSON from the Rust E2E capacity runner.
        #[arg(long)]
        evidence: PathBuf,
        /// Five immutable public scenario preimages, ordered by cold-run ordinal.
        #[arg(long = "scenario", required = true, num_args = 5)]
        scenarios: Vec<PathBuf>,
        /// Exact generated-limits manifest bound into the capacity profile.
        #[arg(long)]
        limits_manifest: PathBuf,
        /// Destination for the deterministic generated capacity manifest.
        #[arg(long)]
        output: PathBuf,
    },
    /// Generate or verify the final chain-bound OCM-26 artifact set.
    FinalArtifacts {
        /// Generated capacity manifest produced from the five cold runs.
        #[arg(long)]
        capacity: PathBuf,
        /// Fresh base genesis without an OCOMP fork-install extension.
        #[arg(long)]
        base_genesis: PathBuf,
        /// Exact four-validator public bootstrap manifest.
        #[arg(long)]
        validators: PathBuf,
        /// Directory containing the complete generated final artifact set.
        #[arg(long)]
        output_dir: PathBuf,
        /// Fail if the existing output differs from deterministic generation.
        #[arg(long)]
        check: bool,
    },
    /// Generate or check the OCOMP V1 object/domain/list registry.
    Registry {
        /// Fail if checked-in generated files differ from the TSV authority.
        #[arg(long)]
        check: bool,
    },
    /// Generate or verify the measurement-only P0 protocol shape freeze.
    Shape {
        /// Fail if checked-in generated shape artifacts differ from their authorities.
        #[arg(long)]
        check: bool,
    },
    /// Run one OCM task merge gate without claiming full PoC closure.
    Task {
        /// Exact task identifier, for example OCM-02.
        task: String,
    },
    /// Execute one exact OCOMP evidence lane without claiming full closure.
    Lane {
        /// Registered execution lane: OCM-FAST, OCM-INT, OCM-PUBLIC, OCM-E2E or OCM-ISO.
        lane: String,
        /// New root for the artifact set and immutable lane evidence.
        #[arg(long, alias = "evidence-dir")]
        output_dir: Option<PathBuf>,
    },
    /// Execute all exact OCM-27 lanes and publish one independently verified closure.
    Closure {
        /// New root retaining the artifact set, lane bundles and closure reports.
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct StablecoinArgs {
    #[command(subcommand)]
    command: StablecoinCommand,
}

#[derive(Debug, Subcommand)]
enum StablecoinCommand {
    /// Compare every checked-in ABI export with its compiled Solidity interface.
    AbiCheck,
    /// Check fixed addresses, planned classes, predeploys and genesis files.
    NamespaceCheck {
        /// Genesis files to validate. May be repeated.
        #[arg(long)]
        genesis: Vec<PathBuf>,
        /// Allow missing fixed marker accounts when checking a pre-seed genesis.
        #[arg(long)]
        preseed: bool,
    },
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

// This command is parsed once and immediately executed. Keeping the manifest paths as
// strongly typed clap fields is clearer than heap-boxing one arbitrary argument solely to
// equalize enum layout.
#[allow(clippy::large_enum_variant)]
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
        cosign_image_verification: PathBuf,
        #[arg(long)]
        cosign_sbom_verification: PathBuf,
        #[arg(long)]
        cosign_provenance_verification: PathBuf,
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
        Command::Ocomp(ocomp_args) => match ocomp_args.command {
            OcompCommand::CapacityBudget { output } => {
                ocomp::capacity::publish_budget(&repo_root, &output)?;
            }
            OcompCommand::CapacityMeasure {
                output_dir,
                limits_manifest,
                generated_capacity,
            } => {
                ocomp::capacity::measure(
                    &repo_root,
                    &output_dir,
                    &limits_manifest,
                    &generated_capacity,
                )?;
            }
            OcompCommand::Capacity {
                evidence,
                scenarios,
                limits_manifest,
                output,
            } => {
                ocomp::capacity::run(&repo_root, &evidence, &scenarios, &limits_manifest, &output)?;
            }
            OcompCommand::FinalArtifacts {
                capacity,
                base_genesis,
                validators,
                output_dir,
                check,
            } => {
                ocomp::finalize::run(
                    &repo_root,
                    &capacity,
                    &base_genesis,
                    &validators,
                    &output_dir,
                    check,
                )?;
            }
            OcompCommand::Registry { check } => {
                ocomp::registry::run(&repo_root, check)?;
            }
            OcompCommand::Shape { check } => {
                ocomp::shape::run(&repo_root, check)?;
            }
            OcompCommand::Task { task } => {
                ocomp::task::run(&repo_root, &task)?;
            }
            OcompCommand::Lane { lane, output_dir } => {
                ocomp::task::run_lane(&repo_root, &lane, output_dir.as_deref())?;
            }
            OcompCommand::Closure { output_dir } => {
                ocomp::task::run_closure(&repo_root, output_dir.as_deref())?;
            }
        },
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
                    cosign_image_verification,
                    cosign_sbom_verification,
                    cosign_provenance_verification,
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
                            cosign_image_verification,
                            cosign_provenance_verification,
                            cosign_sbom_verification,
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
        Command::Stablecoin(stablecoin_args) => match stablecoin_args.command {
            StablecoinCommand::AbiCheck => {
                let report = stablecoin::check_abi_exports(&repo_root)?;
                println!(
                    "stablecoin ABI exports ok: {} complete interfaces",
                    report.interfaces
                );
            }
            StablecoinCommand::NamespaceCheck { genesis, preseed } => {
                let report = stablecoin::check_namespace(&repo_root, &genesis, preseed)?;
                println!(
                    "stablecoin namespace ok: {} declared addresses, {} Ethereum built-ins, {} genesis files, {} seed predeploys, {} planned classes",
                    report.declared_addresses,
                    report.ethereum_builtins,
                    report.genesis_files,
                    report.predeploy_addresses,
                    report.planned_classes,
                );
            }
        },
    }
    Ok(())
}
