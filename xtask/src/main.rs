use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use eyre::{bail, ensure, Result, WrapErr as _};
use xtask::{
    protocol_bench,
    release::{elf, sgx, toolchain},
    stablecoin,
};

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
    /// Run deterministic protocol gas and latency benchmarks.
    ProtocolBench(ProtocolBenchArgs),
}

#[derive(Debug, Args)]
struct ProtocolBenchArgs {
    #[command(subcommand)]
    command: ProtocolBenchCommand,
}

#[derive(Debug, Subcommand)]
enum ProtocolBenchCommand {
    /// Run selected Rust protocol scenarios.
    Run(ProtocolBenchOptions),
    /// Fail when deterministic gas output differs from the checked baseline.
    BaselineCheck(ProtocolBenchOptions),
    /// Explicitly replace the deterministic gas baseline after a validated run.
    BaselineUpdate(ProtocolBenchOptions),
}

#[derive(Debug, Args)]
struct ProtocolBenchOptions {
    /// Samples per selected scenario (minimum 3).
    #[arg(long, default_value_t = 300)]
    samples: usize,
    /// Scenario id substring, or `all`.
    #[arg(long, default_value = "all")]
    filter: String,
    /// Optional machine-readable report destination.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Optional deterministic gas baseline path.
    #[arg(long)]
    baseline: Option<PathBuf>,
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
    /// Validate build specs, generate manifests and compare reproducible ELF outputs.
    Elf(ElfArgs),
    /// Prepare, authorize and verify a pre-signed Gramine SGX bundle.
    Sgx(SgxArgs),
    /// Verify project toolchain pins, APT closures and Intel DCAP native-QVL artifacts.
    Toolchain(ToolchainArgs),
}

#[derive(Debug, Args)]
struct ToolchainArgs {
    #[command(subcommand)]
    command: ToolchainCommand,
}

#[derive(Debug, Subcommand)]
enum ToolchainCommand {
    /// Verify repository toolchain pin contracts across manifests, workflows and scripts.
    VerifyRepo {
        #[arg(long)]
        repo_root: Option<PathBuf>,
    },
    /// Verify exact APT closure debs and ledger against the project toolchain pin.
    VerifyAptInputs {
        #[arg(long)]
        pin: PathBuf,
        #[arg(long)]
        apt_root: PathBuf,
        #[arg(long)]
        deb_dir: PathBuf,
    },
    /// Regenerate the pinned APT closure from the pinned Ubuntu snapshot.
    RegenerateAptClosure {
        #[arg(long)]
        write: bool,
    },
    /// Verify and optionally install the built Intel DCAP native-QVL manifest and artifacts.
    VerifyQvl {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        install_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct ElfArgs {
    #[command(subcommand)]
    command: ElfCommand,
}

#[derive(Debug, Subcommand)]
enum ElfCommand {
    /// Validate the build spec, resolve release identity, or execute the artifact build/check plan.
    Inputs {
        #[arg(long)]
        build_spec: PathBuf,
        #[arg(long)]
        repo_root: PathBuf,
        #[arg(long)]
        release_tag: Option<String>,
        #[arg(long)]
        execute_artifact_build_plan: bool,
        #[arg(long)]
        execute_artifact_check_plan: bool,
    },
    /// Verify that outbe-chain --version output binds the deterministic release identity.
    VerifyVersion {
        #[arg(long)]
        version_file: PathBuf,
        #[arg(long)]
        source_commit: String,
        #[arg(long)]
        source_date_epoch: i64,
        #[arg(long)]
        target: String,
        #[arg(long)]
        profile: String,
    },
    /// Generate a canonical v1 Outbe ReleaseManifest from a reproducible ELF build.
    GenerateManifest {
        #[arg(long)]
        build_spec: PathBuf,
        #[arg(long)]
        source_root: PathBuf,
        #[arg(long)]
        artifact_dir: PathBuf,
        #[arg(long)]
        release_tag: String,
        #[arg(long)]
        source_commit: String,
        #[arg(long)]
        source_date_epoch: i64,
        #[arg(long, default_value = "build-candidate")]
        lifecycle: String,
        #[arg(long)]
        verification_gates: Option<PathBuf>,
        #[arg(long)]
        resolved_system_packages: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare two clean Outbe ELF build outputs and save canonical evidence.
    Compare {
        #[arg(long)]
        first: PathBuf,
        #[arg(long)]
        second: PathBuf,
        #[arg(long)]
        repo_root: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
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
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
        /// Approved seeded ChainSpec. Its chain identity and epoch-0 committee are measured into
        /// the enclave.
        #[arg(long)]
        genesis: PathBuf,
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
    /// Authorize an unsigned bundle with the protected network SGX key.
    Sign {
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
        #[arg(long)]
        unsigned: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Materialize the only allowed final-genesis change from a signed bundle.
    FinalizeGenesis {
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
        /// Approved genesis without teeAttestationV1.
        #[arg(long)]
        seeded_genesis: PathBuf,
        /// Signed SGX bundle whose measurements become the block-1 policy.
        #[arg(long)]
        bundle: PathBuf,
        /// New final genesis. The command never overwrites an existing path.
        #[arg(long)]
        output: PathBuf,
        /// Canonical evidence for the seeded-to-final transformation.
        #[arg(long)]
        evidence_output: PathBuf,
    },
    /// Verify checksums, SIGSTRUCT and the exact final genesis policy binding.
    Verify {
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
        #[arg(long)]
        bundle: PathBuf,
        /// Final genesis whose block-1 policy must authorize this exact bundle.
        #[arg(long)]
        genesis: PathBuf,
    },
    /// Create a deterministic archive from an already verified signed bundle.
    Archive {
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Build an immutable OCI image from an already verified signed bundle.
    Image {
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
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
        #[arg(long, value_enum)]
        network: sgx::SgxReleaseNetwork,
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
        processor_dcap_archive: PathBuf,
        #[arg(long)]
        processor_dcap_evidence: PathBuf,
        /// Approved genesis before the release policy is inserted.
        #[arg(long)]
        seeded_genesis: PathBuf,
        /// Canonical evidence for the seeded-to-final genesis transformation.
        #[arg(long)]
        network_binding_evidence: PathBuf,
        /// Final network genesis whose block-1 policy authorizes this enclave.
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::ProtocolBench(arguments) => {
            let repo_root = sgx::repository_root()?;
            let (action, options) = match arguments.command {
                ProtocolBenchCommand::Run(options) => (protocol_bench::Action::Run, options),
                ProtocolBenchCommand::BaselineCheck(options) => {
                    (protocol_bench::Action::BaselineCheck, options)
                }
                ProtocolBenchCommand::BaselineUpdate(options) => {
                    (protocol_bench::Action::BaselineUpdate, options)
                }
            };
            protocol_bench::run(
                &repo_root,
                action,
                &protocol_bench::Options {
                    samples: options.samples,
                    filter: &options.filter,
                    json: options.json.as_deref(),
                    baseline: options.baseline.as_deref(),
                },
            )?;
        }
        Command::Release(release) => match release.command {
            ReleaseCommand::Elf(elf_args) => run_elf_command(elf_args.command)?,
            ReleaseCommand::Sgx(sgx_args) => {
                let repo_root = sgx::repository_root()?;
                run_sgx_command(&repo_root, sgx_args.command)?;
            }
            ReleaseCommand::Toolchain(toolchain_args) => {
                let repo_root = sgx::repository_root()?;
                run_toolchain_command(&repo_root, toolchain_args.command)?;
            }
        },
        Command::Stablecoin(stablecoin_args) => {
            let repo_root = sgx::repository_root()?;
            match stablecoin_args.command {
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
            }
        }
    }
    Ok(())
}

fn run_elf_command(command: ElfCommand) -> Result<()> {
    match command {
        ElfCommand::Inputs {
            build_spec,
            repo_root,
            release_tag,
            execute_artifact_build_plan,
            execute_artifact_check_plan,
        } => run_elf_inputs(
            &build_spec,
            &repo_root,
            release_tag.as_deref(),
            execute_artifact_build_plan,
            execute_artifact_check_plan,
        ),
        ElfCommand::VerifyVersion {
            version_file,
            source_commit,
            source_date_epoch,
            target,
            profile,
        } => {
            let text = fs::read_to_string(&version_file)
                .wrap_err_with(|| format!("read {}", version_file.display()))?;
            let diffs = elf::verify_version_text(
                &text,
                &source_commit,
                source_date_epoch,
                &target,
                &profile,
            );
            ensure!(
                diffs.is_empty(),
                "outbe-chain version output mismatch: {}",
                diffs.join("; ")
            );
            Ok(())
        }
        ElfCommand::GenerateManifest {
            build_spec,
            source_root,
            artifact_dir,
            release_tag,
            source_commit,
            source_date_epoch,
            lifecycle,
            verification_gates,
            resolved_system_packages,
            output,
        } => {
            let spec: serde_json::Value = serde_json::from_str(&fs::read_to_string(&build_spec)?)?;
            let gates: Vec<serde_json::Value> = match verification_gates {
                Some(path) => serde_json::from_str(&fs::read_to_string(path)?)?,
                None => Vec::new(),
            };
            let manifest = elf::build_manifest(
                &spec,
                &source_root,
                &artifact_dir,
                &elf::ReleaseManifestOptions {
                    release_tag,
                    source_commit,
                    source_date_epoch,
                    lifecycle,
                    verification_gates: gates,
                    resolved_system_packages,
                },
            )?;
            write_json_output(&output, &manifest)
        }
        ElfCommand::Compare {
            first,
            second,
            repo_root,
            output,
        } => {
            let repo_root = match repo_root {
                Some(repo_root) => repo_root,
                None => sgx::repository_root()?,
            };
            run_elf_compare(&first, &second, &repo_root, &output)
        }
    }
}

fn write_json_output(output: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, sgx::canonical_json(value)?)?;
    Ok(())
}

fn run_elf_inputs(
    build_spec: &Path,
    repo_root: &Path,
    release_tag: Option<&str>,
    execute_build: bool,
    execute_check: bool,
) -> Result<()> {
    ensure!(
        !(execute_build && execute_check),
        "choose only one artifact execution plan"
    );
    let spec = elf::load_and_validate_build_spec(build_spec, Some(repo_root))?;
    if execute_build || execute_check {
        ensure!(
            release_tag.is_none(),
            "--release-tag cannot be used with an artifact execution plan"
        );
        return if execute_build {
            elf::execute_artifact_build_plan(&spec)
        } else {
            elf::execute_artifact_check_plan(&spec)
        };
    }
    let identity = elf::resolve_release_identity(repo_root, release_tag)?;
    for value in elf::resolved_values(&spec, &identity)? {
        println!("{value}");
    }
    Ok(())
}

fn run_elf_compare(first: &Path, second: &Path, repo_root: &Path, output: &Path) -> Result<()> {
    let evidence = elf::verify_outputs(first, second, repo_root, true)?;
    write_json_output(output, &evidence)?;
    let result = evidence["result"].as_str().unwrap_or("failed");
    println!("reproducibility result: {result}");
    if let Some(diffs) = evidence["differences"].as_array() {
        for diff in diffs.iter().filter_map(serde_json::Value::as_str) {
            println!("difference: {diff}");
        }
    }
    if result != "passed" {
        bail!("ELF reproducibility comparison failed");
    }
    Ok(())
}

fn run_sgx_command(repo_root: &Path, command: SgxCommand) -> Result<()> {
    match command {
        SgxCommand::Prepare {
            network,
            genesis,
            elf_output,
            output,
        } => {
            sgx::prepare(repo_root, network, &genesis, &elf_output, &output)?;
            println!(
                "unsigned deterministic {network:?} SGX bundle: {}",
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
            network,
            unsigned,
            key_file,
            output,
        } => {
            sgx::sign(repo_root, network, &unsigned, &key_file, &output)?;
            println!("signed {network:?} SGX bundle: {}", output.display());
        }
        SgxCommand::FinalizeGenesis {
            network,
            seeded_genesis,
            bundle,
            output,
            evidence_output,
        } => {
            sgx::finalize_genesis(
                repo_root,
                network,
                &seeded_genesis,
                &bundle,
                (&output, &evidence_output),
            )?;
            println!(
                "final {network:?} genesis: {} (evidence: {})",
                output.display(),
                evidence_output.display()
            );
        }
        SgxCommand::Verify {
            network,
            bundle,
            genesis,
        } => {
            sgx::verify_with_genesis(repo_root, network, &bundle, &genesis)?;
            println!(
                "verified signed {network:?} SGX bundle: {}",
                bundle.display()
            );
        }
        SgxCommand::Archive {
            network,
            bundle,
            output,
        } => {
            sgx::archive(repo_root, network, &bundle, &output)?;
            println!(
                "deterministic signed {network:?} SGX archive: {}",
                output.display()
            );
        }
        SgxCommand::Image {
            network,
            bundle,
            image,
            output,
            push,
        } => {
            sgx::build_image(repo_root, network, &bundle, (&image, push), &output)?;
            println!("{network:?} SGX OCI evidence: {}", output.display());
        }
        SgxCommand::Manifest {
            network,
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
            processor_dcap_archive,
            processor_dcap_evidence,
            seeded_genesis,
            network_binding_evidence,
            genesis,
            output,
        } => {
            sgx::finalize_release_manifest(
                repo_root,
                &sgx::VerifiedReleaseInputs {
                    network,
                    bundle,
                    bundle_archive,
                    cosign_image_verification,
                    cosign_provenance_verification,
                    cosign_sbom_verification,
                    elf_evidence,
                    elf_manifest,
                    hardware_evidence,
                    processor_dcap_archive,
                    processor_dcap_evidence,
                    oci_evidence,
                    sbom,
                    sgx_evidence,
                    seeded_genesis,
                    network_binding_evidence,
                    genesis,
                },
                &output,
            )?;
            println!("verified {network:?} ReleaseManifest: {}", output.display());
        }
    }
    Ok(())
}

fn run_toolchain_command(default_repo_root: &Path, command: ToolchainCommand) -> Result<()> {
    match command {
        ToolchainCommand::VerifyRepo { repo_root } => {
            toolchain::verify_repository(repo_root.as_deref().unwrap_or(default_repo_root))
        }
        ToolchainCommand::VerifyAptInputs {
            pin,
            apt_root,
            deb_dir,
        } => toolchain::verify_apt_inputs(&pin, &apt_root, &deb_dir),
        ToolchainCommand::RegenerateAptClosure { write } => {
            toolchain::regenerate_apt_closure(default_repo_root, write)
        }
        ToolchainCommand::VerifyQvl {
            manifest,
            root,
            install_dir,
        } => {
            let pin = toolchain::load_and_validate_pin(
                &default_repo_root.join("release/project-toolchain-v1.json"),
            )?;
            let manifest_value = toolchain::load_qvl_manifest(&manifest)?;
            if let Some(dir) = install_dir {
                toolchain::install_verified_artifacts(&manifest_value, &pin, &root, &dir, true)
            } else {
                toolchain::verify_qvl_manifest(&manifest_value, &pin, &root, true)
            }
        }
    }
}
