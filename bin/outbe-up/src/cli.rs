use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use eyre::Result;

use crate::{network, validator, vm};

#[derive(Debug, Parser)]
#[command(name = "outbe-up", about = "Outbe node operator CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build and operate a new founding-validator network.
    Network(NetworkArgs),
    /// Prepare, synchronize, promote, and inspect a validator host.
    Validator(ValidatorArgs),
    /// Build and manage Outbe VM data disks and instances.
    Vm(VmArgs),
    #[command(hide = true)]
    Internal(InternalArgs),
}

#[derive(Debug, Args)]
struct NetworkArgs {
    #[command(subcommand)]
    command: NetworkCommand,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Write a documented network configuration template.
    #[command(alias = "init")]
    ConfigExample {
        #[arg(long, default_value = "network.yaml")]
        output: PathBuf,
    },
    /// Build genesis and host-specific artifacts from one immutable software bundle.
    Build {
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Reuse an existing validator-N key tree instead of generating new identities.
        #[arg(long)]
        keys_dir: Option<PathBuf>,
    },
    /// Verify and install every founding host without starting services.
    Prepare {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
    },
    /// Start every founder and wait for finalized consensus.
    Start {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Show the coordinated state of every founding validator.
    Status {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Args)]
struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommand,
}

#[derive(Debug, Subcommand)]
enum ValidatorCommand {
    /// Print a documented operator configuration template, or write it to --output.
    ConfigExample {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify and install an operator bundle without starting services.
    Prepare {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    /// Restore an optional snapshot, start the validator node, and reach tip.
    Sync {
        #[arg(long)]
        snapshot: Option<PathBuf>,
    },
    /// Stake and confirm readiness without restarting the synchronized node.
    Promote,
    /// Show the current lifecycle state.
    Status {
        /// Keep updating until interrupted.
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Args)]
struct VmArgs {
    #[command(subcommand)]
    command: VmCommand,
}

#[derive(Debug, Subcommand)]
enum VmCommand {
    /// Build a deterministic ext4 data disk image for one VM founder.
    Disk {
        #[arg(long)]
        artifacts: PathBuf,
        /// Founder index (0, 1, ...) or name (e.g. validator-0).
        #[arg(long)]
        founder: String,
        #[arg(long)]
        output: PathBuf,
        /// Override data disk size in GiB (defaults to manifest vm.disk_gib).
        #[arg(long)]
        size_gib: Option<u32>,
    },
    /// Create stopped founder VMs on configured VMM hosts and stage their data disks.
    Create {
        #[arg(long)]
        artifacts: PathBuf,
        /// Local guest OS image directory produced by `cargo xtask release vm-image`.
        #[arg(long)]
        image_dir: PathBuf,
        /// Optional VMM image name (defaults to `--image-dir` directory name).
        #[arg(long)]
        image_name: Option<String>,
        /// Optional directory containing pre-built `validator-N.img` raw disks.
        #[arg(long)]
        disks_dir: Option<PathBuf>,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Disable confidential computing (non-TEE QEMU VM).
        #[arg(long)]
        no_tee: bool,
    },
    /// Apply this network's `mac,ip` reservations to host dnsmasq and reload it.
    HostReserve {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Allow creating the dnsmasq hosts file when it does not yet exist.
        #[arg(long)]
        create: bool,
    },
    /// Start recorded founder VMs, wait for finalized consensus, and configure Hyperlane.
    Start {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Stop only the founder VMs recorded in `vm-state.json`.
    Stop {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
    },
    /// Stop and remove only the founder VMs recorded in `vm-state.json`, leaving images intact.
    Remove {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
    },
    /// Show the VMM and consensus status of every recorded founder VM.
    Status {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Args)]
struct InternalArgs {
    #[command(subcommand)]
    command: InternalCommand,
}

#[derive(Debug, Subcommand)]
enum InternalCommand {
    Feeder(InternalBinArgs),
    Hyperlane(InternalBinArgs),
    HyperlaneSetup,
    VmBoot(InternalBinArgs),
    WaitRpc(WaitArgs),
    WaitValidatorDependencies(WaitArgs),
}

#[derive(Debug, Args)]
struct InternalBinArgs {
    /// Directory containing child daemon binaries. Defaults to `/opt/outbe-chain`.
    #[arg(long)]
    bin_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[arg(long, default_value_t = 120)]
    timeout: u64,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Command::Network(args) => match args.command {
                NetworkCommand::ConfigExample { output } => network::config::init(&output),
                NetworkCommand::Build {
                    software_bundle,
                    config,
                    output,
                    keys_dir,
                } => network::build::run(&software_bundle, &config, &output, keys_dir.as_deref()),
                NetworkCommand::Prepare { artifacts, ssh_key } => {
                    network::prepare::run(&artifacts, ssh_key.as_deref()).await
                }
                NetworkCommand::Start {
                    artifacts,
                    ssh_key,
                    timeout,
                } => {
                    network::start::run(
                        &artifacts,
                        ssh_key.as_deref(),
                        std::time::Duration::from_secs(timeout),
                    )
                    .await
                }
                NetworkCommand::Status {
                    artifacts,
                    ssh_key,
                    watch,
                } => network::status::show(&artifacts, ssh_key.as_deref(), watch).await,
            },
            Command::Validator(args) => match args.command {
                ValidatorCommand::ConfigExample { output } => match output {
                    Some(path) => validator::assets::write_config_example(&path),
                    None => {
                        print!("{}", validator::assets::OPERATOR_CONFIG_EXAMPLE);
                        Ok(())
                    }
                },
                ValidatorCommand::Prepare { bundle, config } => {
                    validator::prepare::run(&bundle, &config).await
                }
                ValidatorCommand::Sync { snapshot } => {
                    validator::sync::run(snapshot.as_deref()).await
                }
                ValidatorCommand::Promote => validator::promote::run().await,
                ValidatorCommand::Status { watch } => validator::status::show(watch).await,
            },
            Command::Vm(args) => match args.command {
                VmCommand::Disk {
                    artifacts,
                    founder,
                    output,
                    size_gib,
                } => vm::disk::run(&artifacts, &founder, &output, size_gib),
                VmCommand::Create {
                    artifacts,
                    image_dir,
                    image_name,
                    disks_dir,
                    ssh_key,
                    no_tee,
                } => {
                    vm::lifecycle::create(vm::lifecycle::CreateOptions {
                        artifacts: &artifacts,
                        image_dir: &image_dir,
                        image_name: image_name.as_deref(),
                        disks_dir: disks_dir.as_deref(),
                        ssh_key: ssh_key.as_deref(),
                        no_tee,
                    })
                    .await
                }
                VmCommand::HostReserve {
                    artifacts,
                    ssh_key,
                    create,
                } => vm::lifecycle::host_reserve(&artifacts, ssh_key.as_deref(), create),
                VmCommand::Start {
                    artifacts,
                    ssh_key,
                    timeout,
                } => {
                    vm::lifecycle::start(
                        &artifacts,
                        ssh_key.as_deref(),
                        std::time::Duration::from_secs(timeout),
                    )
                    .await
                }
                VmCommand::Stop { artifacts, ssh_key } => {
                    vm::lifecycle::stop(&artifacts, ssh_key.as_deref()).await
                }
                VmCommand::Remove { artifacts, ssh_key } => {
                    vm::lifecycle::remove(&artifacts, ssh_key.as_deref()).await
                }
                VmCommand::Status {
                    artifacts,
                    ssh_key,
                    watch,
                } => vm::lifecycle::status(&artifacts, ssh_key.as_deref(), watch).await,
            },
            Command::Internal(args) => match args.command {
                InternalCommand::Feeder(args) => {
                    validator::service::feeder(args.bin_dir.as_deref())
                }
                InternalCommand::Hyperlane(args) => {
                    validator::hyperlane::run_service(args.bin_dir.as_deref()).await
                }
                InternalCommand::HyperlaneSetup => validator::hyperlane::setup_founder().await,
                InternalCommand::VmBoot(args) => validator::vm_boot::run(args.bin_dir.as_deref()),
                InternalCommand::WaitRpc(args) => validator::service::wait_rpc(args.timeout).await,
                InternalCommand::WaitValidatorDependencies(args) => {
                    validator::service::wait_validator_dependencies(args.timeout).await
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn lifecycle_commands_parse() {
        for command in ["config-example", "init"] {
            let parsed = Cli::try_parse_from(["outbe-up", "network", command]).unwrap();
            assert!(matches!(
                parsed.command,
                Command::Network(NetworkArgs {
                    command: NetworkCommand::ConfigExample { output }
                }) if output.as_os_str() == "network.yaml"
            ));
        }
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "build",
            "--software-bundle",
            "software.tar.zst",
            "--config",
            "network.yaml",
            "--output",
            "network-artifacts",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "start",
            "--artifacts",
            "network-final",
            "--timeout",
            "900",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "status",
            "--artifacts",
            "network-final",
            "--watch",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "network",
            "prepare",
            "--artifacts",
            "network-final",
            "--ssh-key",
            "operator-key",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "validator",
            "prepare",
            "--bundle",
            "bundle.tar.zst",
            "--config",
            "validator.yaml",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "validator",
            "sync",
            "--snapshot",
            "snapshot.tar",
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "validator", "promote"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "validator", "status", "--watch"]).is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "disk",
            "--artifacts",
            "network-artifacts",
            "--founder",
            "validator-0",
            "--output",
            "validator-0.img",
            "--size-gib",
            "40",
        ])
        .is_ok());
    }

    #[test]
    fn validator_config_example_output_is_optional() {
        for args in [
            vec!["outbe-up", "validator", "config-example"],
            vec![
                "outbe-up",
                "validator",
                "config-example",
                "--output",
                "operator-config.yaml",
            ],
        ] {
            let expected = args.get(4).map(PathBuf::from);
            let parsed = Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                parsed.command,
                Command::Validator(ValidatorArgs {
                    command: ValidatorCommand::ConfigExample { output }
                }) if output == expected
            ));
        }
    }

    #[test]
    fn internal_commands_are_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("internal"));
    }

    #[test]
    fn internal_feeder_and_hyperlane_accept_optional_bin_dir() {
        for subcommand in ["feeder", "hyperlane"] {
            let default_parsed = Cli::try_parse_from(["outbe-up", "internal", subcommand]).unwrap();
            let explicit_parsed = Cli::try_parse_from([
                "outbe-up",
                "internal",
                subcommand,
                "--bin-dir",
                "/usr/local/bin",
            ])
            .unwrap();
            match (subcommand, default_parsed.command, explicit_parsed.command) {
                (
                    "feeder",
                    Command::Internal(InternalArgs {
                        command: InternalCommand::Feeder(default_args),
                    }),
                    Command::Internal(InternalArgs {
                        command: InternalCommand::Feeder(explicit_args),
                    }),
                )
                | (
                    "hyperlane",
                    Command::Internal(InternalArgs {
                        command: InternalCommand::Hyperlane(default_args),
                    }),
                    Command::Internal(InternalArgs {
                        command: InternalCommand::Hyperlane(explicit_args),
                    }),
                ) => {
                    assert_eq!(default_args.bin_dir, None);
                    assert_eq!(
                        explicit_args.bin_dir.as_deref(),
                        Some(std::path::Path::new("/usr/local/bin"))
                    );
                }
                _ => panic!("unexpected parse result for internal {subcommand}"),
            }
        }
        assert!(matches!(
            Cli::try_parse_from([
                "outbe-up",
                "internal",
                "vm-boot",
                "--bin-dir",
                "/usr/local/bin"
            ])
            .unwrap()
            .command,
            Command::Internal(InternalArgs {
                command: InternalCommand::VmBoot(InternalBinArgs { bin_dir: Some(ref dir) })
            }) if dir == std::path::Path::new("/usr/local/bin")
        ));
    }

    #[test]
    fn vm_lifecycle_commands_parse() {
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--artifacts",
            "net-artifacts",
            "--image-dir",
            "outbe-dev-0.1",
            "--image-name",
            "custom-img",
            "--disks-dir",
            "disks",
            "--ssh-key",
            "op.key",
            "--no-tee",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "host-reserve",
            "--artifacts",
            "net-artifacts",
            "--create",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "start",
            "--artifacts",
            "net-artifacts",
            "--timeout",
            "900",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "stop", "--artifacts", "net-artifacts",])
                .is_ok()
        );
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "remove", "--artifacts", "net-artifacts",])
                .is_ok()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "status",
            "--artifacts",
            "net-artifacts",
            "--watch",
        ])
        .is_ok());
    }
}
