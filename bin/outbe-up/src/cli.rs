use std::{net::Ipv4Addr, path::PathBuf, time::Duration};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use eyre::Result;
use outbe_operator_node_role as validator;
use outbe_operator_vm_image as vm_image;

use crate::{disk, host, image, vm};

#[derive(Debug, Parser)]
#[command(name = "outbe-up", about = "Outbe node operator CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare a VM data disk for a single node role.
    Disk(DiskArgs),
    /// Inspect local VM host networking, verify prerequisites, and print NAT/DHCP/firewall recommendations (read-only).
    Host(Box<HostArgs>),
    /// Build, fetch, verify, list, and remove local Outbe guest OS images.
    Image(Box<ImageArgs>),
    /// Prepare, synchronize, promote, and inspect a validator host.
    Validator(ValidatorArgs),
    /// Print outbe-up package version and SHA-256 of the running executable.
    Version {
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Manage local Outbe VM instances over the host VMM Unix socket.
    Vm(Box<VmArgs>),
    #[command(hide = true)]
    Internal(InternalArgs),
}

#[derive(Debug, Args)]
struct DiskArgs {
    #[command(subcommand)]
    command: DiskCommand,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DiskRoleArg {
    Founder,
    #[value(name = "joining-validator")]
    JoiningValidator,
    #[value(name = "full-node")]
    FullNode,
}

impl From<DiskRoleArg> for validator::NodeRole {
    fn from(value: DiskRoleArg) -> Self {
        match value {
            DiskRoleArg::Founder => Self::Founder,
            DiskRoleArg::JoiningValidator => Self::JoiningValidator,
            DiskRoleArg::FullNode => Self::FullNode,
        }
    }
}

#[derive(Debug, Subcommand)]
enum DiskCommand {
    /// Prepare a single-VM ext4 data disk for founder, joining-validator, or full-node.
    #[command(group(
        ArgGroup::new("disk_target")
            .required(true)
            .multiple(false)
            .args(["output", "staging_name", "device"])
    ))]
    Prepare {
        /// Node role for this VM data disk.
        #[arg(long, value_enum)]
        role: DiskRoleArg,
        /// Founder or joining-validator VM data tree directory, tar archive (.tar, .tar.gz, .tar.zst), or `-` for tar stream on stdin.
        #[arg(long, conflicts_with_all = ["config", "keys_from"])]
        source: Option<PathBuf>,
        /// Verified operator bundle (.tar.zst) for joining-validator, full-node, or founder key verification.
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Operator config YAML (joining-validator) or full-node config YAML (full-node).
        #[arg(long, conflicts_with = "source")]
        config: Option<PathBuf>,
        /// Pre-existing keys directory for deterministic joining-validator disk builds.
        #[arg(long, conflicts_with = "source")]
        keys_from: Option<PathBuf>,
        /// Skip outbe-keygen BLS verification for founder disks (default requires a verified operator bundle).
        #[arg(long)]
        skip_key_verify: bool,
        /// Remove stale .outbe-prepare-*, .outbe-disk-*, and .outbe-vm-disk-* staging directories owned by the current UID before staging.
        #[arg(long)]
        cleanup_stale: bool,
        /// Raw data disk size in GiB.
        #[arg(long, default_value_t = outbe_operator_vm::local::DEFAULT_LOCAL_DISK_GIB)]
        size_gib: u32,
        /// Output raw GPT+ext4 disk image path (refuses to overwrite existing files or symlinks).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Stage raw disk atomically at <state-dir>/staging/<name>/data.img (0700 dir, 0600 file; replaces prior staged image).
        #[arg(long)]
        staging_name: Option<String>,
        /// Directory for local state and staging [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state/staging directories (0700/0600).
        #[arg(long)]
        fix_permissions: bool,
        /// Target block device path (not yet supported; see S3 probe / owner decision R-3).
        #[arg(long)]
        device: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Inspect or clean staged VM data disks under <state-dir>/staging.
    #[command(group(
        ArgGroup::new("clean_mode")
            .required(true)
            .multiple(false)
            .args(["staging_name", "stale", "list"])
    ))]
    Clean {
        /// Remove the staged disk directory <state-dir>/staging/<name> (idempotent if absent).
        #[arg(long)]
        staging_name: Option<String>,
        /// Remove unlocked leftover temporary staging directories (.outbe-prepare-*, .outbe-disk-*, .outbe-vm-disk-*) under <state-dir>/staging.
        #[arg(long)]
        stale: bool,
        /// List staged disks currently present under <state-dir>/staging.
        #[arg(long)]
        list: bool,
        /// Directory for local state and staging [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Tighten unsafe permissions on preexisting user-owned state/staging directories (0700/0600).
        #[arg(long)]
        fix_permissions: bool,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct HostArgs {
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// Inspect local bridge, DHCP, firewall, NAT, VMM, and TEE platform state (read-only).
    Status {
        /// Bridge interface name to inspect [default: VMM default_bridge or obr0].
        #[arg(long)]
        bridge: Option<String>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Override system root directory (for local integration tests only).
        #[arg(long, hide = true)]
        sys_root: Option<PathBuf>,
        /// Emit machine-readable JSON output (schema v1).
        #[arg(long)]
        json: bool,
    },
    /// Verify host VM networking prerequisites (no-op when configured; refuses writes without owner design approval).
    Setup {
        /// Bridge interface name to check [default: VMM default_bridge or obr0].
        #[arg(long)]
        bridge: Option<String>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Show planned checks/steps without performing any writes.
        #[arg(long)]
        dry_run: bool,
        /// Override system root directory (for local integration tests only).
        #[arg(long, hide = true)]
        sys_root: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Inspect local outbound NAT egress and print recommended nftables/sysctl rules (read-only; zero writes).
    Nat {
        /// Bridge interface name [default: VMM default_bridge or obr0].
        #[arg(long)]
        bridge: Option<String>,
        /// Override the host uplink interface (defaults to the IPv4 default-route interface).
        #[arg(long)]
        uplink: Option<String>,
        /// Print recommended commands to remove outbe-up managed NAT artifacts (read-only).
        #[arg(long)]
        remove: bool,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Override system root directory (for local integration tests only).
        #[arg(long, hide = true)]
        sys_root: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Verify (read-only) that host networking, static internal IP (outside DHCP pool and unused), and optional published ports are configured for a VM.
    Check {
        /// Local VM name recorded in the local VM state store.
        #[arg(long)]
        vm: String,
        /// Expected static IPv4 address within the bridge subnet (defaults to VM state `intended_ip`).
        #[arg(long)]
        ip: Option<Ipv4Addr>,
        /// Published port mapping `<guest>[:<host>][/(tcp|udp)]` to verify in live nftables (repeatable).
        #[arg(long = "port")]
        ports: Vec<String>,
        /// Expected allowed source IPv4 CIDR or address (repeatable; used for source checks and recommendations).
        #[arg(long = "source")]
        sources: Vec<String>,
        /// Override the host public IPv4 address for DNAT matching.
        #[arg(long)]
        public_ip: Option<Ipv4Addr>,
        /// Bridge interface name [default: VM's recorded bridge].
        #[arg(long)]
        bridge: Option<String>,
        /// Local dstack VMM Unix socket path [default: ~/.dstack-vmm/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Local dstack VMM home directory [default: derived from --vmm-socket or ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Directory for local per-VM state files [default: ${XDG_STATE_HOME:-~/.local/state}/outbe-up].
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Override system root directory (for local integration tests only).
        #[arg(long, hide = true)]
        sys_root: Option<PathBuf>,
        /// Override expected uid:gid owner of dhcp-hosts (only effective with --sys-root in tests).
        #[arg(long, hide = true)]
        expected_hosts_owner: Option<String>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Compute and print `/etc/host-firewall.nft` DNAT and forward rules for one local VM (read-only; detects port conflicts).
    Publish {
        /// Local VM name recorded in the local VM state store.
        #[arg(long)]
        vm: String,
        /// Port mapping `<guest>[:<host>][/(tcp|udp)]` (repeatable; defaults to host==guest and tcp).
        #[arg(long = "port", required = true)]
        ports: Vec<String>,
        /// Allowed source IPv4 CIDR or address (repeatable; defaults to `0.0.0.0/0`).
        #[arg(long = "source")]
        sources: Vec<String>,
        /// Override the host public IPv4 address for DNAT matching.
        #[arg(long)]
        public_ip: Option<Ipv4Addr>,
        /// Bridge interface name [default: VM's recorded bridge].
        #[arg(long)]
        bridge: Option<String>,
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
        /// Override system root directory (for local integration tests only).
        #[arg(long, hide = true)]
        sys_root: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct ImageArgs {
    #[command(subcommand)]
    command: ImageCommand,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ImageFlavorArg {
    Dev,
    Prod,
}

impl From<ImageFlavorArg> for vm_image::Flavor {
    fn from(value: ImageFlavorArg) -> Self {
        match value {
            ImageFlavorArg::Dev => Self::Dev,
            ImageFlavorArg::Prod => Self::Prod,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ImageCommand {
    /// Build a reproducible dstack guest OS image and its deterministic `.tar.zst` archive(s).
    Build {
        /// Clean Outbe repository checkout containing `deploy/outbe-vm/patches/`.
        #[arg(long, default_value = ".")]
        source: PathBuf,
        /// Verified `.tar.zst` software bundle produced by `cargo xtask release build`.
        #[arg(long)]
        software_bundle: PathBuf,
        /// Output directory receiving built image trees, `.tar.zst` archives, and `vm-image-manifest.json`.
        #[arg(long)]
        output: PathBuf,
        /// Comma-separated dstack image flavors to build (`dev`, `prod`).
        #[arg(long, default_value = "dev,prod")]
        flavors: String,
        /// Local path or git URL of the pinned dstack repository.
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
        /// Optional CPU set passed to `docker run --cpuset-cpus` (`DOCKER_CPUSET_CPUS`).
        #[arg(long)]
        cpuset_cpus: Option<String>,
        /// Build parallelism passed to dstack's `repro-build.sh` (`JOBS`).
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// Run two isolated builds and verify byte-identical flavor outputs AND archive SHA-256.
        #[arg(long)]
        check: bool,
        /// Publish built image(s) into the local VMM image store (`<vmm_home>/image/`).
        #[arg(long)]
        publish: bool,
        /// Local dstack VMM home directory for `--publish` [default: ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Download a guest OS image archive over HTTPS, verify its SHA-256 before extraction, and publish it locally.
    Fetch {
        /// Archive HTTPS URL (including presigned S3 HTTPS URLs).
        #[arg(long)]
        url: String,
        /// Expected 64-hex SHA-256 digest of the raw `.tar.zst` archive (verified before decompression).
        #[arg(long)]
        sha256: String,
        /// Optional expected 64-hex `os_image_hash` (`digest.txt`) to cross-check against the archive manifest.
        #[arg(long)]
        os_image_hash: Option<String>,
        /// Local dstack VMM home directory [default: ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Download timeout in seconds.
        #[arg(long, default_value_t = image::default_fetch_timeout_secs())]
        timeout: u64,
        /// Allow plaintext http:// strictly to loopback addresses (for local integration tests only).
        #[arg(long, hide = true)]
        allow_insecure_loopback_http: bool,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Verify a local guest OS image `.tar.zst` archive by SHA-256 before extraction and publish it into `<vmm_home>/image/`.
    Import {
        /// Local `.tar.zst` guest OS image archive path.
        #[arg(long)]
        archive: PathBuf,
        /// Expected 64-hex SHA-256 digest of the raw `.tar.zst` archive (verified before decompression).
        #[arg(long)]
        sha256: String,
        /// Optional expected 64-hex `os_image_hash` (`digest.txt`) to cross-check against the archive manifest.
        #[arg(long)]
        os_image_hash: Option<String>,
        /// Local dstack VMM home directory [default: ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Verify a built or fetched guest OS image directory offline against its manifest and SHA-256 hashes.
    Verify {
        /// Guest OS image directory to verify.
        #[arg(long)]
        dir: PathBuf,
        /// Optional manifest file (`vm-image-manifest.json` or `image-archive-manifest.json`); defaults to `<dir>/image-archive-manifest.json`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Optional flavor selector when verifying against a multi-flavor `vm-image-manifest.json`.
        #[arg(long, value_enum)]
        flavor: Option<ImageFlavorArg>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// List Outbe-managed guest OS images stored in the local VMM image directory.
    List {
        /// Local dstack VMM home directory [default: ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Remove an unreferenced Outbe-managed guest OS image after checking VMM for active or stopped VMs.
    Remove {
        /// Digest-qualified image directory name (e.g. `outbe-vm-dev-<hash16>`).
        #[arg(long)]
        name: String,
        /// Local dstack VMM home directory [default: ~/.dstack-vmm].
        #[arg(long)]
        vmm_home: Option<PathBuf>,
        /// Local dstack VMM Unix socket path [default: <vmm_home>/run/vmm.sock].
        #[arg(long)]
        vmm_socket: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
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

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum VmCommand {
    /// Create one stopped local VM.
    Create {
        /// Local VM name (used as VMM name and state file stem).
        #[arg(long)]
        name: String,
        /// Optional ownership tag persisted in state and VMM annotations (e.g. `outbe-net:<net>:<hash8>:<member>`).
        #[arg(long)]
        owner_tag: Option<String>,
        /// Local guest OS image directory (published under its digest-qualified name).
        #[arg(long)]
        image: PathBuf,
        /// Raw data disk image (from `disk prepare --output`), converted to qcow2 locally.
        #[arg(
            long,
            conflicts_with = "disk_staging",
            required_unless_present = "disk_staging"
        )]
        disk: Option<PathBuf>,
        /// Staged raw data disk name under <state-dir>/staging/<name>/data.img (from `disk prepare --staging-name`).
        #[arg(long, conflicts_with = "disk", required_unless_present = "disk")]
        disk_staging: Option<String>,
        /// Delete <state-dir>/staging/<name> ONLY after the VM reaches Ready (requires `--disk-staging`).
        #[arg(long, requires = "disk_staging", conflicts_with = "disk")]
        consume_staging: bool,
        /// Host bridge interface to attach (e.g. `obr0`).
        #[arg(long)]
        bridge: String,
        /// Number of vCPUs [default: 8].
        #[arg(long)]
        vcpu: Option<u32>,
        /// Memory in GiB [default: 16].
        #[arg(long)]
        memory_gib: Option<u32>,
        /// Virtual disk size in GiB [default: 100].
        #[arg(long)]
        disk_gib: Option<u32>,
        /// Custom dstack app-compose JSON path (defaults to embedded deploy/outbe-vm/app-compose.json).
        #[arg(long)]
        app_compose: Option<PathBuf>,
        /// Optional intended static IPv4 recorded in state.
        #[arg(long)]
        ip: Option<Ipv4Addr>,
        /// Image flavor label (`dev` or `prod`) for derived image name [default: dev].
        #[arg(long)]
        flavor: Option<String>,
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
        /// Issue a new CreateVm if an unresolved Creating intent did not appear within the grace window.
        #[arg(long)]
        retry_create: bool,
        /// Maximum seconds for lock wait, Creating recovery grace polling, and VMM RPCs.
        #[arg(long)]
        timeout: Option<u64>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
        /// Optional VMM image name (defaults to digest-qualified `outbe-vm-<flavor>-<hash16>`).
        #[arg(long)]
        image_name: Option<String>,
        /// Disable confidential computing (non-TEE QEMU VM).
        #[arg(long)]
        no_tee: bool,
    },
    /// Start one local VM.
    Start {
        /// Local VM name to start.
        #[arg(long)]
        name: String,
        /// Expected VMM VM UUID verified under the per-VM lock before issuing any VMM RPC.
        #[arg(long)]
        expect_vm_id: Option<String>,
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
    /// Stop one local VM gracefully via ShutdownVm.
    Stop {
        /// Local VM name to stop.
        #[arg(long)]
        name: String,
        /// Expected VMM VM UUID verified under the per-VM lock before issuing any VMM RPC.
        #[arg(long)]
        expect_vm_id: Option<String>,
        /// Force-stop via StopVm (bypasses guest shutdown; warns about pending-DKG snapshot defect).
        #[arg(long)]
        force: bool,
        /// Maximum seconds to wait for the VM to stop [default: 120].
        #[arg(long)]
        timeout: Option<u64>,
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
    /// Print the tail of one local VM's serial log.
    Logs {
        /// Local VM name.
        #[arg(long)]
        name: String,
        /// Expected VMM VM UUID verified under the per-VM lock before reading logs.
        #[arg(long)]
        expect_vm_id: Option<String>,
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
    /// Remove one local VM, leaving guest images intact.
    Remove {
        /// Local VM name to remove.
        #[arg(long)]
        name: String,
        /// Expected VMM VM UUID verified under the per-VM lock before issuing any VMM RPC.
        #[arg(long)]
        expect_vm_id: Option<String>,
        /// Force-stop if running before removal (warns about pending-DKG snapshot defect).
        #[arg(long)]
        force: bool,
        /// Delete an unresolved Creating intent when no token-matched VM appeared within the grace window.
        #[arg(long)]
        forget_intent: bool,
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
        /// Maximum seconds to wait for asynchronous VMM removal to complete.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Show status of one or all local VMs.
    Status {
        /// Local VM name (omit to show all locally tracked VMs).
        #[arg(long)]
        name: Option<String>,
        /// Expected VMM VM UUID verified under the per-VM lock before querying VMM (requires `--name`).
        #[arg(long, requires = "name")]
        expect_vm_id: Option<String>,
        /// Path to the host dnsmasq DHCP leases file [default: /var/lib/validator-vm/dhcp-leases].
        #[arg(long)]
        dhcp_leases: Option<PathBuf>,
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
    /// Gracefully stop one local VM, switch its guest OS image via VMM UpdateVm, and start it with the same data disk (never uses forced stop).
    Update {
        /// Local VM name to update.
        #[arg(long)]
        name: String,
        /// Expected VMM VM UUID verified under the per-VM lock before issuing any VMM RPC.
        #[arg(long)]
        expect_vm_id: Option<String>,
        /// Target guest OS image directory (containing metadata.json + digest.txt) or already-published image name under `<vmm-home>/image/`.
        #[arg(long)]
        image: PathBuf,
        /// Override the digest-qualified VMM image name (defaults to `outbe-vm-<flavor>-<digest16>` or published name).
        #[arg(long)]
        image_name: Option<String>,
        /// Guest image flavor label used when deriving the default image name [default: dev].
        #[arg(long)]
        flavor: Option<String>,
        /// Maximum seconds to wait for graceful guest shutdown (`ShutdownVm`; forced stop is never used).
        #[arg(long, default_value_t = 120)]
        stop_timeout: u64,
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
            Command::Disk(args) => match args.command {
                DiskCommand::Prepare {
                    role,
                    source,
                    bundle,
                    config,
                    keys_from,
                    skip_key_verify,
                    cleanup_stale,
                    size_gib,
                    output,
                    staging_name,
                    state_dir,
                    fix_permissions,
                    device,
                    json,
                } => {
                    disk::prepare(disk::PrepareArgs {
                        role: role.into(),
                        source,
                        bundle,
                        config,
                        keys_from,
                        skip_key_verify,
                        cleanup_stale,
                        size_gib,
                        output,
                        staging_name,
                        state_dir,
                        fix_permissions,
                        device,
                        json,
                    })
                    .await
                }
                DiskCommand::Clean {
                    staging_name,
                    stale,
                    list,
                    state_dir,
                    fix_permissions,
                    json,
                } => disk::clean(disk::CleanArgs {
                    staging_name,
                    stale,
                    list,
                    state_dir,
                    fix_permissions,
                    json,
                }),
            },
            Command::Host(args) => match args.command {
                HostCommand::Status {
                    bridge,
                    vmm_socket,
                    vmm_home,
                    sys_root,
                    json,
                } => host::status(host::StatusArgs {
                    bridge,
                    vmm_socket,
                    vmm_home,
                    sys_root,
                    json,
                }),
                HostCommand::Setup {
                    bridge,
                    vmm_socket,
                    vmm_home,
                    dry_run,
                    sys_root,
                    json,
                } => host::setup(host::SetupArgs {
                    bridge,
                    vmm_socket,
                    vmm_home,
                    dry_run,
                    sys_root,
                    json,
                }),
                HostCommand::Nat {
                    bridge,
                    uplink,
                    remove,
                    vmm_socket,
                    vmm_home,
                    sys_root,
                    json,
                } => host::nat(host::NatArgs {
                    bridge,
                    uplink,
                    remove,
                    vmm_socket,
                    vmm_home,
                    sys_root,
                    json,
                }),
                HostCommand::Check {
                    vm,
                    ip,
                    ports,
                    sources,
                    public_ip,
                    bridge,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    sys_root,
                    expected_hosts_owner,
                    json,
                } => host::check(host::CheckArgs {
                    vm,
                    ip,
                    ports,
                    sources,
                    public_ip,
                    bridge,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    sys_root,
                    expected_hosts_owner,
                    json,
                }),
                HostCommand::Publish {
                    vm,
                    ports,
                    sources,
                    public_ip,
                    bridge,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions: _,
                    sys_root,
                    json,
                } => host::publish(host::PublishArgs {
                    vm,
                    ports,
                    sources,
                    public_ip,
                    bridge,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    sys_root,
                    json,
                }),
            },
            Command::Version { json } => run_version(json),
            Command::Image(args) => match args.command {
                ImageCommand::Build {
                    source,
                    software_bundle,
                    output,
                    flavors,
                    dstack_repo,
                    runtime_libs_dir,
                    toolchain_image,
                    mkosi_image,
                    cpuset_cpus,
                    jobs,
                    check,
                    publish,
                    vmm_home,
                    json,
                } => image::build(image::BuildArgs {
                    source,
                    software_bundle,
                    output,
                    flavors,
                    dstack_repo,
                    runtime_libs_dir,
                    toolchain_image,
                    mkosi_image,
                    cpuset_cpus,
                    jobs,
                    check,
                    publish,
                    vmm_home,
                    json,
                }),
                ImageCommand::Fetch {
                    url,
                    sha256,
                    os_image_hash,
                    vmm_home,
                    timeout,
                    allow_insecure_loopback_http,
                    json,
                } => {
                    image::fetch(image::FetchArgs {
                        url,
                        sha256,
                        os_image_hash,
                        vmm_home,
                        timeout_secs: timeout,
                        allow_insecure_loopback_http,
                        json,
                    })
                    .await
                }
                ImageCommand::Import {
                    archive,
                    sha256,
                    os_image_hash,
                    vmm_home,
                    json,
                } => {
                    image::import(image::ImportArgs {
                        archive,
                        sha256,
                        os_image_hash,
                        vmm_home,
                        json,
                    })
                    .await
                }
                ImageCommand::Verify {
                    dir,
                    manifest,
                    flavor,
                    json,
                } => image::verify(image::VerifyArgs {
                    dir,
                    manifest,
                    flavor: flavor.map(Into::into),
                    json,
                }),
                ImageCommand::List { vmm_home, json } => {
                    image::list(image::ListArgs { vmm_home, json })
                }
                ImageCommand::Remove {
                    name,
                    vmm_home,
                    vmm_socket,
                    json,
                } => image::remove(image::RemoveArgs {
                    name,
                    vmm_home,
                    vmm_socket,
                    json,
                }),
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
                VmCommand::Create {
                    name,
                    owner_tag,
                    image,
                    disk,
                    disk_staging,
                    consume_staging,
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
                } => {
                    vm::local::create(vm::local::LocalCreateArgs {
                        name,
                        owner_tag,
                        image,
                        image_name,
                        flavor,
                        disk,
                        disk_staging,
                        consume_staging,
                        vcpu,
                        memory_gib,
                        disk_gib,
                        bridge,
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
                VmCommand::Start {
                    name,
                    expect_vm_id,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => vm::local::start(
                    &name,
                    expect_vm_id.as_deref(),
                    vm::local::LocalConnectionArgs {
                        vmm_socket,
                        vmm_home,
                        state_dir,
                        fix_permissions,
                        json,
                    },
                ),
                VmCommand::Stop {
                    name,
                    expect_vm_id,
                    force,
                    timeout,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => vm::local::stop(
                    &name,
                    expect_vm_id.as_deref(),
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
                VmCommand::Logs {
                    name,
                    expect_vm_id,
                    lines,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => vm::local::logs(
                    &name,
                    expect_vm_id.as_deref(),
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
                    expect_vm_id,
                    force,
                    forget_intent,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                    timeout,
                } => vm::local::remove(vm::local::LocalRemoveArgs {
                    name,
                    expect_vm_id,
                    force,
                    forget_intent,
                    timeout: Duration::from_secs(timeout),
                    conn: vm::local::LocalConnectionArgs {
                        vmm_socket,
                        vmm_home,
                        state_dir,
                        fix_permissions,
                        json,
                    },
                }),
                VmCommand::Status {
                    name,
                    expect_vm_id,
                    dhcp_leases,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => vm::local::status(
                    name.as_deref(),
                    expect_vm_id.as_deref(),
                    dhcp_leases.as_deref(),
                    vm::local::LocalConnectionArgs {
                        vmm_socket,
                        vmm_home,
                        state_dir,
                        fix_permissions,
                        json,
                    },
                ),
                VmCommand::Update {
                    name,
                    expect_vm_id,
                    image,
                    image_name,
                    flavor,
                    stop_timeout,
                    vmm_socket,
                    vmm_home,
                    state_dir,
                    fix_permissions,
                    json,
                } => {
                    vm::local::update(vm::local::LocalUpdateArgs {
                        name,
                        expect_vm_id,
                        image,
                        image_name,
                        flavor,
                        stop_timeout,
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

#[derive(Debug, serde::Serialize)]
struct VersionOutput {
    name: &'static str,
    version: &'static str,
    binary_path: PathBuf,
    binary_sha256: String,
}

fn run_version(json: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let sha256 = vm_image::archive::sha256_file_streaming(&exe)?;
    let out = VersionOutput {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        binary_path: exe,
        binary_sha256: sha256,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "{} {} (sha256: {})",
            out.name, out.version, out.binary_sha256
        );
    }
    Ok(())
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
        // Multi-host `network` and legacy `vm --artifacts` subcommands have moved to `outbe-net-up`.
        assert!(Cli::try_parse_from(["outbe-up", "network", "config-example"]).is_err());
        assert!(Cli::try_parse_from(["outbe-up", "vm", "disk", "--artifacts", "net"]).is_err());
        assert!(
            Cli::try_parse_from(["outbe-up", "vm", "host-reserve", "--artifacts", "net"]).is_err()
        );
        assert!(Cli::try_parse_from(["outbe-up", "vm", "stop", "--artifacts", "net"]).is_err());
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
    fn local_vm_commands_parse_and_reject_missing_bridge_or_removed_legacy_flags() {
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

        // Removed multi-founder `--artifacts` and SSH flags must fail cleanly.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--artifacts",
            "net",
            "--image-dir",
            "img",
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
            Cli::try_parse_from(["outbe-up", "vm", "status", "--name", "dev", "--watch"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "remove",
            "--name",
            "dev",
            "--release-reservations",
        ])
        .is_err());
    }

    #[test]
    fn disk_prepare_commands_parse_and_enforce_target_mutual_exclusion() {
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--output",
            "./validator-0.img",
            "--size-gib",
            "100",
            "--json",
        ])
        .is_ok());
        let default_disk = Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--output",
            "./validator-0.img",
        ])
        .unwrap();
        assert!(matches!(
            default_disk.command,
            Command::Disk(DiskArgs {
                command: DiskCommand::Prepare { size_gib: 100, .. }
            })
        ));
        assert_eq!(outbe_operator_vm::local::DEFAULT_LOCAL_VCPU, 8);
        assert_eq!(outbe_operator_vm::local::DEFAULT_LOCAL_MEMORY_GIB, 16);
        assert_eq!(outbe_operator_vm::local::DEFAULT_LOCAL_DISK_GIB, 100);
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "joining-validator",
            "--bundle",
            "./bundle.tar.zst",
            "--config",
            "./operator.yaml",
            "--keys-from",
            "./keys",
            "--output",
            "./joining.img",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "full-node",
            "--bundle",
            "./bundle.tar.zst",
            "--config",
            "./fullnode.yaml",
            "--device",
            "/dev/loop99",
        ])
        .is_ok());

        // Neither --output nor --device fails.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
        ])
        .is_err());
        // Both --output and --device fail.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--output",
            "./out.img",
            "--device",
            "/dev/loop99",
        ])
        .is_err());
        // Founder accepts --bundle (for verified outbe-keygen) or --skip-key-verify + --cleanup-stale.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--bundle",
            "./bundle.tar.zst",
            "--cleanup-stale",
            "--output",
            "./out.img",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--skip-key-verify",
            "--output",
            "./out.img",
        ])
        .is_ok());
        // Mixing --source with --config or --keys-from fails.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--config",
            "./operator.yaml",
            "--output",
            "./out.img",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "./vm/validator-0",
            "--keys-from",
            "./keys",
            "--output",
            "./out.img",
        ])
        .is_err());
    }

    #[test]
    fn image_commands_parse() {
        assert!(Cli::try_parse_from([
            "outbe-up",
            "image",
            "build",
            "--software-bundle",
            "./bundle.tar.zst",
            "--output",
            "./out-img",
            "--flavors",
            "dev,prod",
            "--check",
            "--publish",
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "image",
            "fetch",
            "--url",
            "https://example.com/image.tar.zst?X-Amz-Signature=abc",
            "--sha256",
            &"a".repeat(64),
            "--os-image-hash",
            &"b".repeat(64),
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "image",
            "verify",
            "--dir",
            "./out/dev/dstack-dev-0.6.3",
            "--manifest",
            "./vm-image-manifest.json",
            "--flavor",
            "dev",
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "image",
            "list",
            "--vmm-home",
            "/tmp/vmm",
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "image",
            "remove",
            "--name",
            "outbe-vm-dev-0123456789abcdef",
            "--vmm-home",
            "/tmp/vmm",
            "--vmm-socket",
            "/tmp/vmm/run/vmm.sock",
            "--json",
        ])
        .is_ok());
    }

    #[test]
    fn host_commands_parse() {
        assert!(Cli::try_parse_from(["outbe-up", "host", "status", "--json"]).is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "host",
            "setup",
            "--bridge",
            "obr0",
            "--dry-run",
            "--json"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "outbe-up", "host", "nat", "--bridge", "obr0", "--remove", "--json"
        ])
        .is_ok());
        // `host reserve` and `host release` are removed (VMs use static IPv4 via OUTBE_VM_IP).
        assert!(Cli::try_parse_from([
            "outbe-up",
            "host",
            "reserve",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--json"
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["outbe-up", "host", "release", "--vm", "node-a", "--json"])
                .is_err()
        );
        // Host write flags (`--apply`, `--force`) are removed (all `outbe-up host ...` commands are strictly read-only).
        assert!(
            Cli::try_parse_from(["outbe-up", "host", "nat", "--bridge", "obr0", "--force"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "host",
            "check",
            "--vm",
            "node-a",
            "--ip",
            "10.77.0.88",
            "--port",
            "30303:30313",
            "--port",
            "30303:30313/udp",
            "--source",
            "198.51.100.30/32",
            "--public-ip",
            "198.51.100.10",
            "--json"
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["outbe-up", "host", "check", "--vm", "node-a", "--json"]).is_ok()
        );
        assert!(Cli::try_parse_from([
            "outbe-up",
            "host",
            "publish",
            "--vm",
            "node-a",
            "--port",
            "9000:9010",
            "--port",
            "30303/udp",
            "--source",
            "198.51.100.30/32",
            "--json"
        ])
        .is_ok());
        // `host publish` requires at least one `--port`.
        assert!(Cli::try_parse_from(["outbe-up", "host", "publish", "--vm", "node-a"]).is_err());
    }

    #[test]
    fn c1_staging_owner_tag_expect_vm_id_and_disk_clean_parse() {
        // disk prepare --source - --staging-name
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--bundle",
            "operator-bundle.tar.zst",
            "--staging-name",
            "validator-0",
            "--state-dir",
            "/tmp/state",
            "--json",
        ])
        .is_ok());
        // --output and --staging-name are mutually exclusive.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--output",
            "a.img",
            "--staging-name",
            "validator-0",
        ])
        .is_err());

        // disk clean modes (--staging-name / --stale / --list) are mutually exclusive, one required.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "disk",
            "clean",
            "--staging-name",
            "validator-0",
            "--json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "disk", "clean", "--stale", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "disk", "clean", "--list", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["outbe-up", "disk", "clean"]).is_err());
        assert!(Cli::try_parse_from(["outbe-up", "disk", "clean", "--stale", "--list"]).is_err());

        // vm create with --disk-staging + --consume-staging + --owner-tag
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "validator-0",
            "--owner-tag",
            "outbe-net:devnet:01234567:validator-0",
            "--image",
            "./img",
            "--disk-staging",
            "validator-0",
            "--consume-staging",
            "--bridge",
            "obr0",
            "--json",
        ])
        .is_ok());
        // --disk and --disk-staging conflict; --consume-staging requires --disk-staging.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "validator-0",
            "--image",
            "./img",
            "--disk",
            "a.img",
            "--disk-staging",
            "validator-0",
            "--bridge",
            "obr0",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "validator-0",
            "--image",
            "./img",
            "--disk",
            "a.img",
            "--consume-staging",
            "--bridge",
            "obr0",
        ])
        .is_err());
        // Local mode requires either --disk or --disk-staging.
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "create",
            "--name",
            "validator-0",
            "--image",
            "./img",
            "--bridge",
            "obr0",
        ])
        .is_err());

        // --expect-vm-id on start, stop, remove, logs, status (--name required on status).
        let uuid = "550e8400-e29b-41d4-a716-446655440010";
        for sub in ["start", "stop", "remove", "logs", "status"] {
            assert!(
                Cli::try_parse_from([
                    "outbe-up",
                    "vm",
                    sub,
                    "--name",
                    "validator-0",
                    "--expect-vm-id",
                    uuid,
                    "--json",
                ])
                .is_ok(),
                "vm {sub} --expect-vm-id failed to parse"
            );
        }
        assert!(Cli::try_parse_from([
            "outbe-up",
            "vm",
            "status",
            "--expect-vm-id",
            uuid,
            "--json",
        ])
        .is_err());
    }
}
