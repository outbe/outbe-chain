use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use eyre::{bail, ensure, Result, WrapErr as _};
use xtask::{
    layer_boundaries, protocol_bench,
    release::{canonical_json, elf, repository_root, software, toolchain, vm_image},
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
    /// Enforce crates/blockchain dependency and source-inclusion layer boundaries.
    CheckLayerBoundaries {
        #[arg(long)]
        repo_root: Option<PathBuf>,
    },
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
    /// Build a deterministic, network-neutral Outbe software bundle.
    Build(SoftwareBuildArgs),
    /// Build a reproducible dstack guest VM image from a verified software bundle.
    VmImage(VmImageArgs),
    /// Validate build specs, generate manifests and compare reproducible ELF outputs.
    Elf(ElfArgs),
    /// Verify project toolchain pins, APT closures and Intel DCAP native-QVL artifacts.
    Toolchain(ToolchainArgs),
}

#[derive(Debug, Args)]
struct VmImageArgs {
    /// Verified `.tar.zst` software bundle produced by `cargo xtask release build`.
    #[arg(long)]
    software_bundle: PathBuf,
    /// Directory that receives the built VM image artifacts and `vm-image-manifest.json`.
    #[arg(long)]
    output: PathBuf,
    /// Comma-separated dstack image flavors to build (`dev`, `prod`).
    #[arg(long, default_value = "dev,prod")]
    flavors: String,
    /// Local path or git URL of the dstack repository.
    #[arg(long, default_value = vm_image::DEFAULT_DSTACK_REPO)]
    dstack_repo: String,
    /// Directory containing pinned LLVM 18 runtime libraries (`libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`).
    #[arg(long)]
    runtime_libs_dir: Option<PathBuf>,
    /// Docker image tag for the extracted toolchain stage.
    #[arg(long, default_value = vm_image::DEFAULT_TOOLCHAIN_IMAGE)]
    toolchain_image: String,
    /// Docker image tag for dstack's mkosi build container (`DSTACK_MKOSI_IMAGE`).
    #[arg(long, default_value = vm_image::DEFAULT_MKOSI_IMAGE)]
    mkosi_image: String,
    /// Optional CPU set passed to `docker run --cpuset-cpus` (`DOCKER_CPUSET_CPUS`, e.g. `0-3`).
    #[arg(long)]
    cpuset_cpus: Option<String>,
    /// Build parallelism passed to dstack's `repro-build.sh` (`JOBS`).
    #[arg(long, default_value_t = 4)]
    jobs: usize,
    /// Run dstack `repro-check` (build twice and compare byte for byte).
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Args)]
struct SoftwareBuildArgs {
    /// Directory that receives the versioned `.tar.zst` bundle.
    #[arg(long)]
    output: PathBuf,
    /// Immutable release tag. Defaults to `commit-<full-sha>`.
    #[arg(long)]
    release_tag: Option<String>,
    /// Disable the Docker build cache when producing independent evidence.
    #[arg(long)]
    no_cache: bool,
    /// Internal half of the pinned container build.
    #[arg(long, hide = true)]
    inside_container: bool,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::CheckLayerBoundaries { repo_root } => {
            let root = repo_root.unwrap_or_else(default_workspace_root);
            let report = layer_boundaries::check_layer_boundaries(&root)?;
            println!(
                "crates/blockchain layer boundaries ok: {} packages, {} Rust source files, {} verified inclusions",
                report.blockchain_packages, report.scanned_source_files, report.verified_inclusions,
            );
        }
        Command::ProtocolBench(arguments) => {
            let repo_root = repository_root()?;
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
            ReleaseCommand::Build(arguments) => {
                let repo_root = if arguments.inside_container {
                    std::env::current_dir()?
                } else {
                    repository_root()?
                };
                let output = software::build(&software::BuildOptions {
                    repo_root: &repo_root,
                    output_dir: &arguments.output,
                    release_tag: arguments.release_tag.as_deref(),
                    no_cache: arguments.no_cache,
                    inside_container: arguments.inside_container,
                })?;
                println!("software bundle: {}", output.display());
            }
            ReleaseCommand::VmImage(arguments) => {
                let repo_root = repository_root()?;
                let flavors = vm_image::parse_flavors(&arguments.flavors)?;
                let manifest = vm_image::build(&vm_image::VmImageOptions {
                    repo_root: &repo_root,
                    software_bundle: &arguments.software_bundle,
                    output_dir: &arguments.output,
                    flavors: &flavors,
                    dstack_repo: &arguments.dstack_repo,
                    runtime_libs_dir: arguments.runtime_libs_dir.as_deref(),
                    toolchain_image: &arguments.toolchain_image,
                    mkosi_image: &arguments.mkosi_image,
                    cpuset_cpus: arguments.cpuset_cpus.as_deref(),
                    jobs: arguments.jobs,
                    check: arguments.check,
                })?;
                println!(
                    "vm image manifest: {}",
                    arguments.output.join(vm_image::MANIFEST_NAME).display()
                );
                for (flavor, entry) in &manifest.flavors {
                    println!("  {flavor:<6} os_image_hash={}", entry.os_image_hash);
                }
            }
            ReleaseCommand::Elf(elf_args) => run_elf_command(elf_args.command)?,
            ReleaseCommand::Toolchain(toolchain_args) => {
                let repo_root = repository_root()?;
                run_toolchain_command(&repo_root, toolchain_args.command)?;
            }
        },
        Command::Stablecoin(stablecoin_args) => {
            let repo_root = repository_root()?;
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
                None => repository_root()?,
            };
            run_elf_compare(&first, &second, &repo_root, &output)
        }
    }
}

fn write_json_output(output: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, canonical_json(value)?)?;
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

fn default_workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("xtask is one level below the repository root")
        .to_owned()
}
