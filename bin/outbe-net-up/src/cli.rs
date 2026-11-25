use std::{net::Ipv4Addr, path::PathBuf, time::Duration};

use clap::{ArgGroup, Args, Parser, Subcommand};
use eyre::{bail, Result};

use crate::network;

#[derive(Debug, Parser)]
#[command(
    name = "outbe-net-up",
    about = "Outbe multi-host founding-network orchestrator"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build and operate a founding-validator network across one or more hosts.
    Network(NetworkArgs),
}

#[derive(Debug, Args)]
struct NetworkArgs {
    #[command(subcommand)]
    command: NetworkCommand,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Write an example network configuration file.
    #[command(alias = "init")]
    ConfigExample {
        #[arg(long, default_value = "network.yaml")]
        output: PathBuf,
    },
    /// Generate keys, seeded/final genesis, founder artifacts, and joining bundle.
    Build {
        #[arg(long)]
        software_bundle: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        keys_dir: Option<PathBuf>,
    },
    /// Deploy built network artifacts across hosts (VM mode reconciles against host `outbe-up --json`; bare-host mode runs prepare).
    #[command(group(
        ArgGroup::new("image_source")
            .args(["image_archive", "image_url"])
            .multiple(false)
    ))]
    Deploy {
        #[arg(long)]
        artifacts: PathBuf,
        /// Local guest OS image `.tar.zst` archive to upload and import on each VM host.
        #[arg(long)]
        image_archive: Option<PathBuf>,
        /// HTTPS URL of a guest OS image `.tar.zst` archive for hosts to download via `outbe-up image fetch`.
        #[arg(long, requires = "image_sha256")]
        image_url: Option<String>,
        /// Expected 64-hex SHA-256 of the guest OS image `.tar.zst` archive.
        #[arg(long)]
        image_sha256: Option<String>,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Pass `--no-tee` to `outbe-up vm create` (for non-TEE development hosts only).
        #[arg(long)]
        no_tee: bool,
    },
    /// Prepare bare-host founding validators without starting them (R-5).
    Prepare {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
    },
    /// Start founding validators and wait for finalized consensus growth.
    Start {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Maximum acceptable execution-to-finalized block lag during readiness checks.
        #[arg(long, default_value_t = 20)]
        max_lag: u64,
        /// Minimum finalized block growth (`min(finalized) >= B + N` across two consecutive samples).
        #[arg(long, default_value_t = 10)]
        min_finalized_blocks: u64,
    },
    /// Stop all founding validator VMs for this network.
    Stop {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Force-stop VMs that do not shut down gracefully.
        #[arg(long)]
        force: bool,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Inspect founding validator health and consensus status (works on full or partial deploys).
    Status {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        #[arg(long)]
        watch: bool,
        /// Maximum acceptable execution-to-finalized block lag.
        #[arg(long, default_value_t = 20)]
        max_lag: u64,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Remove founding validator VMs and staged disks gracefully (never modifies host DHCP/firewall config).
    Remove {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Force-stop running VMs before removal (bypasses graceful guest shutdown).
        #[arg(long)]
        force: bool,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Add a joining validator VM to a running VM network and drive it through DKG activation.
    #[command(group(
        ArgGroup::new("join_image_source")
            .args(["image_archive", "image_url"])
            .multiple(false)
    ))]
    AddValidator {
        #[arg(long)]
        artifacts: PathBuf,
        /// Unique member name (e.g. `validator-joiner-0`; VM name becomes `<network>-<name>`).
        #[arg(long)]
        name: String,
        /// Target VMM host SSH hostname or IP.
        #[arg(long)]
        host: String,
        /// Target VMM host SSH user [default: network vm default or `ubuntu`].
        #[arg(long)]
        user: Option<String>,
        /// Target VMM host SSH port [default: 22].
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Target VMM home directory [default: network vm.vmm_home].
        #[arg(long)]
        vmm_home: Option<String>,
        /// Target host bridge name [default: network vm.bridge].
        #[arg(long)]
        bridge: Option<String>,
        /// Static private IPv4 address assigned to this VM on the host bridge.
        #[arg(long)]
        bridge_ip: Ipv4Addr,
        /// Public/external IPv4 address advertised for P2P and on-chain consensus registration.
        #[arg(long)]
        advertise_ip: Ipv4Addr,
        /// Port offset added to base P2P/Radicle ports (1:1 host-to-guest DNAT).
        #[arg(long, default_value_t = 0)]
        port_offset: u16,
        /// Local guest OS image `.tar.zst` archive (defaults to already-deployed image if omitted).
        #[arg(long)]
        image_archive: Option<PathBuf>,
        /// HTTPS URL of a guest OS image `.tar.zst` archive.
        #[arg(long, requires = "image_sha256")]
        image_url: Option<String>,
        /// Expected 64-hex SHA-256 of the guest OS image `.tar.zst` archive.
        #[arg(long)]
        image_sha256: Option<String>,
        #[arg(long)]
        vcpu: Option<u32>,
        #[arg(long)]
        memory_gib: Option<u32>,
        #[arg(long)]
        disk_gib: Option<u32>,
        /// Pass `--no-tee` to `outbe-up vm create` (for non-TEE development hosts only).
        #[arg(long)]
        no_tee: bool,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Overall timeout in seconds for sync, stake, DKG activation, and post-activation lockstep.
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// Maximum acceptable finalized block lag.
        #[arg(long, default_value_t = 20)]
        max_lag: u64,
        /// Minimum post-activation finalized block growth across two consecutive samples.
        #[arg(long, default_value_t = 10)]
        min_finalized_blocks: u64,
    },
    /// Add a strictly keyless full-node VM to a running VM network and verify finalized sync.
    #[command(group(
        ArgGroup::new("fullnode_image_source")
            .args(["image_archive", "image_url"])
            .multiple(false)
    ))]
    AddFullNode {
        #[arg(long)]
        artifacts: PathBuf,
        /// Unique member name (e.g. `fullnode-0`; VM name becomes `<network>-<name>`).
        #[arg(long)]
        name: String,
        /// Target VMM host SSH hostname or IP.
        #[arg(long)]
        host: String,
        /// Target VMM host SSH user [default: network vm default or `ubuntu`].
        #[arg(long)]
        user: Option<String>,
        /// Target VMM host SSH port [default: 22].
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Target VMM home directory [default: network vm.vmm_home].
        #[arg(long)]
        vmm_home: Option<String>,
        /// Target host bridge name [default: network vm.bridge].
        #[arg(long)]
        bridge: Option<String>,
        /// Static private IPv4 address assigned to this VM on the host bridge.
        #[arg(long)]
        bridge_ip: Ipv4Addr,
        /// Optional external IPv4 written to validator.env (defaults to bridge_ip; outbound-only).
        #[arg(long)]
        advertise_ip: Option<Ipv4Addr>,
        /// Local guest OS image `.tar.zst` archive (defaults to already-deployed image if omitted).
        #[arg(long)]
        image_archive: Option<PathBuf>,
        /// HTTPS URL of a guest OS image `.tar.zst` archive.
        #[arg(long, requires = "image_sha256")]
        image_url: Option<String>,
        /// Expected 64-hex SHA-256 of the guest OS image `.tar.zst` archive.
        #[arg(long)]
        image_sha256: Option<String>,
        #[arg(long)]
        vcpu: Option<u32>,
        #[arg(long)]
        memory_gib: Option<u32>,
        #[arg(long)]
        disk_gib: Option<u32>,
        /// Pass `--no-tee` to `outbe-up vm create` (for non-TEE development hosts only).
        #[arg(long)]
        no_tee: bool,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null`).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Overall timeout in seconds for sync and finalized tracking verification.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Maximum acceptable finalized block lag relative to validators.
        #[arg(long, default_value_t = 20)]
        max_lag: u64,
        /// Minimum finalized block growth tracked with identical block hashes across two consecutive samples.
        #[arg(long, default_value_t = 10)]
        min_finalized_blocks: u64,
    },
    /// Rolling guest OS image upgrade across a running network (planned for S7).
    Update {
        #[arg(long)]
        artifacts: PathBuf,
        /// Target guest OS image name or archive path.
        #[arg(long)]
        image: String,
    },
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
                NetworkCommand::Deploy {
                    artifacts,
                    image_archive,
                    image_url,
                    image_sha256,
                    ssh_key,
                    ssh_config,
                    no_tee,
                } => {
                    let loaded = network::remote::NetworkArtifacts::load(&artifacts)?;
                    match loaded.manifest.deployment {
                        network::build::DeploymentKind::Host => {
                            network::prepare::run(
                                &artifacts,
                                ssh_key.as_deref(),
                                ssh_config.as_deref(),
                            )
                            .await
                        }
                        network::build::DeploymentKind::Vm => {
                            let image_source = match (image_archive, image_url) {
                                (Some(path), None) => network::vm::ImageSource::Archive {
                                    path,
                                    sha256: image_sha256,
                                },
                                (None, Some(url)) => network::vm::ImageSource::Url {
                                    url,
                                    sha256: image_sha256.expect(
                                        "clap requires --image-sha256 when --image-url is set",
                                    ),
                                },
                                (None, None) => {
                                    bail!(
                                        "VM deployment requires either `--image-archive <path>` or `--image-url <https-url> --image-sha256 <hex>`"
                                    );
                                }
                                (Some(_), Some(_)) => {
                                    unreachable!("clap enforces mutual exclusion on image_source");
                                }
                            };
                            network::vm::deploy(network::vm::DeployOptions {
                                artifacts: &artifacts,
                                image_source,
                                ssh_key: ssh_key.as_deref(),
                                ssh_config: ssh_config.as_deref(),
                                no_tee,
                            })
                            .await
                        }
                    }
                }
                NetworkCommand::Prepare {
                    artifacts,
                    ssh_key,
                    ssh_config,
                } => {
                    network::prepare::run(&artifacts, ssh_key.as_deref(), ssh_config.as_deref())
                        .await
                }
                NetworkCommand::Start {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    timeout,
                    max_lag,
                    min_finalized_blocks,
                } => {
                    let loaded = network::remote::NetworkArtifacts::load(&artifacts)?;
                    match loaded.manifest.deployment {
                        network::build::DeploymentKind::Host => {
                            network::start::run(
                                &artifacts,
                                ssh_key.as_deref(),
                                ssh_config.as_deref(),
                                Duration::from_secs(timeout),
                            )
                            .await
                        }
                        network::build::DeploymentKind::Vm => {
                            network::vm::start(network::vm::StartOptions {
                                artifacts: &artifacts,
                                ssh_key: ssh_key.as_deref(),
                                ssh_config: ssh_config.as_deref(),
                                timeout: Duration::from_secs(timeout),
                                max_lag,
                                min_finalized_blocks,
                            })
                            .await
                        }
                    }
                }
                NetworkCommand::Stop {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    force,
                    timeout,
                } => {
                    let loaded = network::remote::NetworkArtifacts::load(&artifacts)?;
                    match loaded.manifest.deployment {
                        network::build::DeploymentKind::Vm => {
                            network::vm::stop(network::vm::StopOptions {
                                artifacts: &artifacts,
                                ssh_key: ssh_key.as_deref(),
                                ssh_config: ssh_config.as_deref(),
                                force,
                                timeout: Duration::from_secs(timeout),
                            })
                            .await
                        }
                        network::build::DeploymentKind::Host => {
                            bail!(
                                "`outbe-net-up network stop` currently supports VM deployments only"
                            );
                        }
                    }
                }
                NetworkCommand::Status {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    watch,
                    max_lag,
                    json,
                } => {
                    let loaded = network::remote::NetworkArtifacts::load(&artifacts)?;
                    match loaded.manifest.deployment {
                        network::build::DeploymentKind::Host => {
                            network::status::show(
                                &artifacts,
                                ssh_key.as_deref(),
                                ssh_config.as_deref(),
                                watch,
                                json,
                            )
                            .await
                        }
                        network::build::DeploymentKind::Vm => {
                            network::vm::status(network::vm::StatusOptions {
                                artifacts: &artifacts,
                                ssh_key: ssh_key.as_deref(),
                                ssh_config: ssh_config.as_deref(),
                                watch,
                                max_lag,
                                json,
                            })
                            .await
                        }
                    }
                }
                NetworkCommand::Remove {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    force,
                    timeout,
                } => {
                    let loaded = network::remote::NetworkArtifacts::load(&artifacts)?;
                    match loaded.manifest.deployment {
                        network::build::DeploymentKind::Vm => {
                            network::vm::remove(network::vm::RemoveOptions {
                                artifacts: &artifacts,
                                ssh_key: ssh_key.as_deref(),
                                ssh_config: ssh_config.as_deref(),
                                force,
                                timeout: Duration::from_secs(timeout),
                            })
                            .await
                        }
                        network::build::DeploymentKind::Host => {
                            bail!(
                                "`outbe-net-up network remove` currently supports VM deployments only"
                            );
                        }
                    }
                }
                NetworkCommand::AddValidator {
                    artifacts,
                    name,
                    host,
                    user,
                    port,
                    vmm_home,
                    bridge,
                    bridge_ip,
                    advertise_ip,
                    port_offset,
                    image_archive,
                    image_url,
                    image_sha256,
                    vcpu,
                    memory_gib,
                    disk_gib,
                    no_tee,
                    ssh_key,
                    ssh_config,
                    timeout,
                    max_lag,
                    min_finalized_blocks,
                } => {
                    let image_source =
                        parse_optional_image_source(image_archive, image_url, image_sha256);
                    network::vm::add_validator(network::vm::AddValidatorOptions {
                        artifacts: &artifacts,
                        name: &name,
                        host: &host,
                        user: user.as_deref(),
                        port,
                        vmm_home: vmm_home.as_deref(),
                        bridge: bridge.as_deref(),
                        bridge_ip,
                        advertise_ip,
                        port_offset,
                        image_source,
                        vcpu,
                        memory_gib,
                        disk_gib,
                        no_tee,
                        ssh_key: ssh_key.as_deref(),
                        ssh_config: ssh_config.as_deref(),
                        timeout: Duration::from_secs(timeout),
                        max_lag,
                        min_finalized_blocks,
                    })
                    .await
                }
                NetworkCommand::AddFullNode {
                    artifacts,
                    name,
                    host,
                    user,
                    port,
                    vmm_home,
                    bridge,
                    bridge_ip,
                    advertise_ip,
                    image_archive,
                    image_url,
                    image_sha256,
                    vcpu,
                    memory_gib,
                    disk_gib,
                    no_tee,
                    ssh_key,
                    ssh_config,
                    timeout,
                    max_lag,
                    min_finalized_blocks,
                } => {
                    let image_source =
                        parse_optional_image_source(image_archive, image_url, image_sha256);
                    network::vm::add_full_node(network::vm::AddFullNodeOptions {
                        artifacts: &artifacts,
                        name: &name,
                        host: &host,
                        user: user.as_deref(),
                        port,
                        vmm_home: vmm_home.as_deref(),
                        bridge: bridge.as_deref(),
                        bridge_ip,
                        advertise_ip,
                        image_source,
                        vcpu,
                        memory_gib,
                        disk_gib,
                        no_tee,
                        ssh_key: ssh_key.as_deref(),
                        ssh_config: ssh_config.as_deref(),
                        timeout: Duration::from_secs(timeout),
                        max_lag,
                        min_finalized_blocks,
                    })
                    .await
                }
                NetworkCommand::Update { artifacts, image } => {
                    bail!(
                        "`outbe-net-up network update` (artifacts={}, image={image}) is not yet implemented (planned for S7 rolling VM image upgrade)",
                        artifacts.display()
                    );
                }
            },
        }
    }
}

fn parse_optional_image_source(
    image_archive: Option<PathBuf>,
    image_url: Option<String>,
    image_sha256: Option<String>,
) -> Option<network::vm::ImageSource> {
    match (image_archive, image_url) {
        (Some(path), None) => Some(network::vm::ImageSource::Archive {
            path,
            sha256: image_sha256,
        }),
        (None, Some(url)) => Some(network::vm::ImageSource::Url {
            url,
            sha256: image_sha256.expect("clap requires --image-sha256 when --image-url is set"),
        }),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap enforces mutual exclusion"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_passes_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn network_subcommands_parse_cleanly() {
        for sub in ["config-example", "init"] {
            assert!(Cli::try_parse_from(["outbe-net-up", "network", sub]).is_ok());
        }
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "build",
            "--software-bundle",
            "bundle.tar.zst",
            "--config",
            "network.yaml",
            "--output",
            "artifacts",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "deploy",
            "--artifacts",
            "artifacts",
            "--image-archive",
            "image.tar.zst",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "deploy",
            "--artifacts",
            "artifacts",
            "--image-url",
            "https://example.invalid/image.tar.zst",
            "--image-sha256",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ])
        .is_ok());
        // Removed flags (`--wait-host-check`, `--configure-hyperlane`, `--release-reservations`) must be rejected.
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "deploy",
            "--artifacts",
            "artifacts",
            "--image-archive",
            "image.tar.zst",
            "--wait-host-check",
        ])
        .is_err());
        let parsed_start = Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "start",
            "--artifacts",
            "artifacts",
        ])
        .unwrap();
        assert!(matches!(
            parsed_start.command,
            Command::Network(NetworkArgs {
                command: NetworkCommand::Start {
                    min_finalized_blocks: 10,
                    ..
                }
            })
        ));
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "start",
            "--artifacts",
            "artifacts",
            "--configure-hyperlane",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "stop",
            "--artifacts",
            "artifacts",
            "--force",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "status",
            "--artifacts",
            "artifacts",
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "remove",
            "--artifacts",
            "artifacts",
            "--force",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "remove",
            "--artifacts",
            "artifacts",
            "--release-reservations",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "add-validator",
            "--artifacts",
            "artifacts",
            "--name",
            "validator-joiner-0",
            "--host",
            "141.95.168.110",
            "--bridge-ip",
            "10.77.0.13",
            "--advertise-ip",
            "141.95.168.110",
            "--port-offset",
            "2",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "add-full-node",
            "--artifacts",
            "artifacts",
            "--name",
            "fullnode-0",
            "--host",
            "162.19.107.121",
            "--bridge-ip",
            "10.77.0.13",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-net-up",
            "network",
            "update",
            "--artifacts",
            "artifacts",
            "--image",
            "outbe-vm-dev-0123456789abcdef",
        ])
        .is_ok());
    }
}
