use std::{net::Ipv4Addr, path::PathBuf, time::Duration};

use clap::{ArgGroup, Args, Parser, Subcommand};
use eyre::Result;
use outbe_operator_node_role as validator;

use crate::{network, vm};

const LEGACY_VM_HEADING: &str =
    "Legacy multi-founder mode (--artifacts, moves to outbe-net-up in S6)";
const LEGACY_VM_NOTICE: &str = "network-level vm commands move to outbe-net-up";

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
    Vm(Box<VmArgs>),
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
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
    },
    /// Start every founder and wait for finalized consensus.
    Start {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Show the coordinated state of every founding validator.
    Status {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
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
    /// Create one stopped local VM (or legacy founder VMs when `--artifacts` is given).
    #[command(
        group(ArgGroup::new("local_opts").multiple(true).conflicts_with("legacy_opts")),
        group(ArgGroup::new("legacy_opts").multiple(true).conflicts_with("local_opts")),
    )]
    Create {
        /// Local VM name (used as VMM name and state file stem).
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        name: Option<String>,
        /// Local guest OS image directory (published under its digest-qualified name).
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        image: Option<PathBuf>,
        /// Raw data disk image (from `vm disk` or `disk prepare`), converted to qcow2 locally.
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        disk: Option<PathBuf>,
        /// Host bridge interface to attach (required in local mode, e.g. `obr0`).
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        bridge: Option<String>,
        /// Number of vCPUs [default: 3].
        #[arg(long, group = "local_opts")]
        vcpu: Option<u32>,
        /// Memory in GiB [default: 5].
        #[arg(long, group = "local_opts")]
        memory_gib: Option<u32>,
        /// Virtual disk size in GiB [default: 40].
        #[arg(long, group = "local_opts")]
        disk_gib: Option<u32>,
        /// Custom dstack app-compose JSON path (defaults to embedded deploy/outbe-vm/app-compose.json).
        #[arg(long, group = "local_opts")]
        app_compose: Option<PathBuf>,
        /// Optional intended static IPv4 recorded in state (reservation itself is handled in S2).
        #[arg(long, group = "local_opts")]
        ip: Option<Ipv4Addr>,
        /// Image flavor label (`dev` or `prod`) for derived image name [default: dev].
        #[arg(long, group = "local_opts")]
        flavor: Option<String>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long, group = "local_opts")]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long, group = "local_opts")]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long, group = "local_opts")]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long, group = "local_opts")]
        fix_permissions: bool,
        /// Issue a new CreateVm if an unresolved Creating intent did not appear within the grace window.
        #[arg(long, group = "local_opts")]
        retry_create: bool,
        /// Maximum seconds for lock wait, Creating recovery grace polling, and VMM RPCs.
        #[arg(long, group = "local_opts")]
        timeout: Option<u64>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long, group = "local_opts")]
        json: bool,
        /// Optional VMM image name (defaults to digest-qualified `outbe-vm-<flavor>-<hash16>`).
        #[arg(long)]
        image_name: Option<String>,
        /// Disable confidential computing (non-TEE QEMU VM).
        #[arg(long)]
        no_tee: bool,
        /// Legacy multi-founder network artifacts directory (runs over SSH).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "image_dir")]
        artifacts: Option<PathBuf>,
        /// Local guest OS image directory produced by `cargo xtask release vm-image`.
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        image_dir: Option<PathBuf>,
        /// Optional directory containing pre-built `validator-N.img` raw disks.
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        disks_dir: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_config: Option<PathBuf>,
    },
    /// Apply this network's `mac,ip` reservations to host dnsmasq and reload it.
    HostReserve {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Allow creating the dnsmasq hosts file when it does not yet exist.
        #[arg(long)]
        create: bool,
    },
    /// Remove only this network's managed dnsmasq reservations and reload dnsmasq.
    HostRelease {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
    },
    /// Configure or remove host-side NAT masquerade so VM guests have outbound internet access.
    HostNat {
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long)]
        ssh_config: Option<PathBuf>,
        /// Override the host uplink interface (defaults to the IPv4 default-route interface).
        #[arg(long)]
        uplink: Option<String>,
        /// Remove the managed nftables table, systemd unit, ruleset file, and sysctl drop-in (if ours).
        #[arg(long)]
        remove: bool,
        /// Print preflight findings, rendered nftables ruleset, and planned commands without modifying the host.
        #[arg(long)]
        dry_run: bool,
        /// Apply even when existing foreign egress is detected or foreign forward chains cannot be verified statically.
        #[arg(long)]
        force: bool,
    },
    /// Start one local VM (or legacy founder VMs when `--artifacts` is given).
    #[command(
        group(ArgGroup::new("local_opts").multiple(true).conflicts_with("legacy_opts")),
        group(ArgGroup::new("legacy_opts").multiple(true).conflicts_with("local_opts")),
    )]
    Start {
        /// Local VM name to start.
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        name: Option<String>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long, group = "local_opts")]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long, group = "local_opts")]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long, group = "local_opts")]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long, group = "local_opts")]
        fix_permissions: bool,
        /// Emit machine-readable JSON on stdout.
        #[arg(long, group = "local_opts")]
        json: bool,
        /// Legacy multi-founder network artifacts directory (runs over SSH).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING)]
        artifacts: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_config: Option<PathBuf>,
        /// Maximum seconds to wait for finalized consensus in legacy mode [default: 600].
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        timeout: Option<u64>,
        /// Maximum allowed finalized-block lag to the highest founder before flagging lagging [default: 20].
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        max_lag: Option<u64>,
    },
    /// Stop one local VM gracefully via ShutdownVm (or legacy founder VMs when `--artifacts` is given).
    #[command(
        group(ArgGroup::new("local_opts").multiple(true).conflicts_with("legacy_opts")),
        group(ArgGroup::new("legacy_opts").multiple(true).conflicts_with("local_opts")),
    )]
    Stop {
        /// Local VM name to stop.
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        name: Option<String>,
        /// Force-stop via StopVm (bypasses guest shutdown; warns about pending-DKG snapshot defect).
        #[arg(long, group = "local_opts")]
        force: bool,
        /// Maximum seconds to wait for the VM to stop [default: 120].
        #[arg(long, group = "local_opts")]
        timeout: Option<u64>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long, group = "local_opts")]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long, group = "local_opts")]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long, group = "local_opts")]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long, group = "local_opts")]
        fix_permissions: bool,
        /// Emit machine-readable JSON on stdout.
        #[arg(long, group = "local_opts")]
        json: bool,
        /// Legacy multi-founder network artifacts directory (runs over SSH).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING)]
        artifacts: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_config: Option<PathBuf>,
    },
    /// Print the tail of one local VM's serial log.
    Logs {
        /// Local VM name.
        #[arg(long)]
        name: String,
        /// Number of trailing lines to print.
        #[arg(long, default_value_t = 100)]
        lines: usize,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long)]
        fix_permissions: bool,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Remove one local VM (or legacy founder VMs when `--artifacts` is given), leaving guest images intact.
    #[command(
        group(ArgGroup::new("local_opts").multiple(true).conflicts_with("legacy_opts")),
        group(ArgGroup::new("legacy_opts").multiple(true).conflicts_with("local_opts")),
    )]
    Remove {
        /// Local VM name to remove.
        #[arg(long, group = "local_opts", required_unless_present = "artifacts")]
        name: Option<String>,
        /// Force-stop if running before removal (warns about pending-DKG snapshot defect).
        #[arg(long, group = "local_opts")]
        force: bool,
        /// Delete an unresolved Creating intent when no token-matched VM appeared within the grace window.
        #[arg(long, group = "local_opts")]
        forget_intent: bool,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long, group = "local_opts")]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long, group = "local_opts")]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long, group = "local_opts")]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long, group = "local_opts")]
        fix_permissions: bool,
        /// Emit machine-readable JSON on stdout.
        #[arg(long, group = "local_opts")]
        json: bool,
        /// Maximum seconds to wait for asynchronous VMM removal to complete.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Legacy multi-founder network artifacts directory (runs over SSH).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING)]
        artifacts: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_config: Option<PathBuf>,
        /// Also release this network's managed dnsmasq reservations after VMs are removed.
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        release_reservations: bool,
    },
    /// Show status of one or all local VMs (or legacy founder VMs when `--artifacts` is given).
    #[command(
        group(ArgGroup::new("local_opts").multiple(true).conflicts_with("legacy_opts")),
        group(ArgGroup::new("legacy_opts").multiple(true).conflicts_with("local_opts")),
    )]
    Status {
        /// Local VM name (omit to show all locally tracked VMs).
        #[arg(long, group = "local_opts")]
        name: Option<String>,
        /// Path to the host dnsmasq DHCP leases file [default: /var/lib/validator-vm/dhcp-leases].
        #[arg(long, group = "local_opts")]
        dhcp_leases: Option<PathBuf>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long, group = "local_opts")]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long, group = "local_opts")]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long, group = "local_opts")]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state directories/files (0700/0600).
        #[arg(long, group = "local_opts")]
        fix_permissions: bool,
        /// Emit machine-readable JSON on stdout.
        #[arg(long, group = "local_opts")]
        json: bool,
        /// Legacy multi-founder network artifacts directory (runs over SSH).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING)]
        artifacts: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_key: Option<PathBuf>,
        /// Path to an SSH config file passed via `-F` (e.g. `--ssh-config /dev/null` to ignore system SSH config).
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        ssh_config: Option<PathBuf>,
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        watch: bool,
        /// Maximum allowed finalized-block lag to the highest founder before flagging lagging [default: 20].
        #[arg(long, group = "legacy_opts", help_heading = LEGACY_VM_HEADING, requires = "artifacts")]
        max_lag: Option<u64>,
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
                } => {
                    network::start::run(
                        &artifacts,
                        ssh_key.as_deref(),
                        ssh_config.as_deref(),
                        Duration::from_secs(timeout),
                    )
                    .await
                }
                NetworkCommand::Status {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    watch,
                } => {
                    network::status::show(
                        &artifacts,
                        ssh_key.as_deref(),
                        ssh_config.as_deref(),
                        watch,
                    )
                    .await
                }
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
                } => {
                    eprintln!("{LEGACY_VM_NOTICE}");
                    vm::disk::run(&artifacts, &founder, &output, size_gib)
                }
                VmCommand::Create {
                    name,
                    image,
                    disk,
                    bridge,
                    vcpu,
                    memory_gib,
                    disk_gib,
                    app_compose,
                    ip,
                    flavor,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    retry_create,
                    timeout,
                    json,
                    image_name,
                    no_tee,
                    artifacts,
                    image_dir,
                    disks_dir,
                    ssh_key,
                    ssh_config,
                } => match artifacts {
                    Some(artifacts) => {
                        eprintln!("{LEGACY_VM_NOTICE}");
                        let image_dir =
                            image_dir.expect("clap enforces --image-dir with --artifacts");
                        vm::lifecycle::create(vm::lifecycle::CreateOptions {
                            artifacts: &artifacts,
                            image_dir: &image_dir,
                            image_name: image_name.as_deref(),
                            disks_dir: disks_dir.as_deref(),
                            ssh_key: ssh_key.as_deref(),
                            ssh_config: ssh_config.as_deref(),
                            no_tee,
                        })
                        .await
                    }
                    None => {
                        vm::local::create(vm::local::LocalCreateArgs {
                            name: name.expect("clap enforces --name without --artifacts"),
                            image: image.expect("clap enforces --image without --artifacts"),
                            image_name,
                            flavor,
                            disk: disk.expect("clap enforces --disk without --artifacts"),
                            vcpu,
                            memory_gib,
                            disk_gib,
                            bridge: bridge.expect("clap enforces --bridge without --artifacts"),
                            app_compose,
                            ip,
                            no_tee,
                            retry_create,
                            timeout,
                            conn: vm::local::LocalConnectionArgs {
                                vmm_socket,
                                vmm_home,
                                state_dir,
                                fix_permissions,
                                json,
                            },
                        })
                        .await
                    }
                },
                VmCommand::HostReserve {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    create,
                } => {
                    eprintln!("{LEGACY_VM_NOTICE}");
                    vm::lifecycle::host_reserve(
                        &artifacts,
                        ssh_key.as_deref(),
                        ssh_config.as_deref(),
                        create,
                    )
                }
                VmCommand::HostRelease {
                    artifacts,
                    ssh_key,
                    ssh_config,
                } => {
                    eprintln!("{LEGACY_VM_NOTICE}");
                    vm::lifecycle::host_release(
                        &artifacts,
                        ssh_key.as_deref(),
                        ssh_config.as_deref(),
                    )
                }
                VmCommand::HostNat {
                    artifacts,
                    ssh_key,
                    ssh_config,
                    uplink,
                    remove,
                    dry_run,
                    force,
                } => {
                    eprintln!("{LEGACY_VM_NOTICE}");
                    vm::nat::run(vm::nat::HostNatOptions {
                        artifacts: &artifacts,
                        ssh_key: ssh_key.as_deref(),
                        ssh_config: ssh_config.as_deref(),
                        uplink: uplink.as_deref(),
                        remove,
                        dry_run,
                        force,
                    })
                }
                VmCommand::Start {
                    name,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                    artifacts,
                    ssh_key,
                    ssh_config,
                    timeout,
                    max_lag,
                } => match artifacts {
                    Some(artifacts) => {
                        eprintln!("{LEGACY_VM_NOTICE}");
                        vm::lifecycle::start(
                            &artifacts,
                            ssh_key.as_deref(),
                            ssh_config.as_deref(),
                            Duration::from_secs(timeout.unwrap_or(600)),
                            max_lag.unwrap_or(20),
                        )
                        .await
                    }
                    None => vm::local::start(
                        &name.expect("clap enforces --name without --artifacts"),
                        vm::local::LocalConnectionArgs {
                            vmm_socket,
                            vmm_home,
                            state_dir,
                            fix_permissions,
                            json,
                        },
                    ),
                },
                VmCommand::Stop {
                    name,
                    force,
                    timeout,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                    artifacts,
                    ssh_key,
                    ssh_config,
                } => match artifacts {
                    Some(artifacts) => {
                        eprintln!("{LEGACY_VM_NOTICE}");
                        vm::lifecycle::stop(&artifacts, ssh_key.as_deref(), ssh_config.as_deref())
                            .await
                    }
                    None => vm::local::stop(
                        &name.expect("clap enforces --name without --artifacts"),
                        force,
                        Duration::from_secs(timeout.unwrap_or(120)),
                        vm::local::LocalConnectionArgs {
                            vmm_socket,
                            vmm_home,
                            state_dir,
                            fix_permissions,
                            json,
                        },
                    ),
                },
                VmCommand::Logs {
                    name,
                    lines,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => vm::local::logs(
                    &name,
                    lines,
                    vm::local::LocalConnectionArgs {
                        vmm_socket,
                        vmm_home,
                        state_dir,
                        fix_permissions,
                        json,
                    },
                ),
                VmCommand::Remove {
                    name,
                    force,
                    forget_intent,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                    timeout,
                    artifacts,
                    ssh_key,
                    ssh_config,
                    release_reservations,
                } => match artifacts {
                    Some(artifacts) => {
                        eprintln!("{LEGACY_VM_NOTICE}");
                        vm::lifecycle::remove(
                            &artifacts,
                            ssh_key.as_deref(),
                            ssh_config.as_deref(),
                            release_reservations,
                            Duration::from_secs(timeout),
                        )
                        .await
                    }
                    None => vm::local::remove(
                        &name.expect("clap enforces --name without --artifacts"),
                        force,
                        forget_intent,
                        Duration::from_secs(timeout),
                        vm::local::LocalConnectionArgs {
                            vmm_socket,
                            vmm_home,
                            state_dir,
                            fix_permissions,
                            json,
                        },
                    ),
                },
                VmCommand::Status {
                    name,
                    dhcp_leases,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                    artifacts,
                    ssh_key,
                    ssh_config,
                    watch,
                    max_lag,
                } => match artifacts {
                    Some(artifacts) => {
                        eprintln!("{LEGACY_VM_NOTICE}");
                        vm::lifecycle::status(
                            &artifacts,
                            ssh_key.as_deref(),
                            ssh_config.as_deref(),
                            watch,
                            max_lag.unwrap_or(20),
                        )
                        .await
                    }
                    None => vm::local::status(
                        name.as_deref(),
                        dhcp_leases.as_deref(),
                        vm::local::LocalConnectionArgs {
                            vmm_socket,
                            vmm_home,
                            state_dir,
                            fix_permissions,
                            json,
                        },
                    ),
                },
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
    fn clap_definition_passes_debug_assert() {
        Cli::command().debug_assert();
    }

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
            "--ssh-config",
            "/dev/null",
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
            "--ssh-config",
            "/dev/null",
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
            "host-release",
            "--artifacts",
            "net-artifacts",
            "--ssh-config",
            "/dev/null",
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
            "--max-lag",
            "20",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "stop", "--artifacts", "net-artifacts",])
                .is_ok()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--artifacts",
            "net-artifacts",
            "--release-reservations",
            "--timeout",
            "180",
        ])
        .is_ok());
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

    #[test]
    fn local_vm_commands_parse_and_reject_missing_bridge_or_mixed_legacy_flags() {
        // Valid local invocations across all 6 subcommands.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "dev",
            "--image",
            "./guest-img",
            "--disk",
            "./dev.img",
            "--bridge",
            "obr0",
            "--vcpu",
            "3",
            "--memory-gib",
            "5",
            "--disk-gib",
            "10",
            "--ip",
            "10.77.0.88",
            "--retry-create",
            "--timeout",
            "90",
            "--json",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "start", "--name", "dev", "--json"]).is_ok()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "stop",
            "--name",
            "dev",
            "--force",
            "--timeout",
            "30",
            "--json",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "status", "--name", "dev", "--json"]).is_ok()
        );
        assert!(Cli::try_parse_from(["outbe-up", "vm", "status", "--json"]).is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up", "vm", "logs", "--name", "dev", "--lines", "50", "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--name",
            "dev",
            "--forget-intent",
            "--timeout",
            "60",
            "--json",
        ])
        .is_ok());

        // --bridge is strictly required in local `vm create` (no silent default).
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "dev",
            "--image",
            "./guest-img",
            "--disk",
            "./dev.img",
        ])
        .is_err());

        // Mutual exclusion: mixing --artifacts with local flags fails in clap across every dual-mode command.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--artifacts",
            "net",
            "--image-dir",
            "img",
            "--name",
            "dev",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "dev",
            "--image",
            "img",
            "--disk",
            "d.img",
            "--bridge",
            "obr0",
            "--ssh-key",
            "k",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "start",
            "--artifacts",
            "net",
            "--name",
            "dev",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "start",
            "--name",
            "dev",
            "--ssh-config",
            "/dev/null",
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "stop", "--artifacts", "net", "--force",])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "stop", "--artifacts", "net", "--json",])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "status",
            "--artifacts",
            "net",
            "--name",
            "dev",
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "status", "--name", "dev", "--watch",]).is_err()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--artifacts",
            "net",
            "--name",
            "dev",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--name",
            "dev",
            "--release-reservations",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--artifacts",
            "net",
            "--image-dir",
            "img",
            "--retry-create",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--artifacts",
            "net",
            "--forget-intent",
        ])
        .is_err());
    }
}
