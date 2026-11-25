//! Multi-host VM founding-network orchestration (`outbe-net-up network deploy|start|stop|status|remove`).
//!
//! Architecture (S6.3):
//! - Host `outbe-up vm status --json` is always the source of truth; `network-vm-state.json` (v2,
//!   under `flock`) stores desired/cached metadata and NO VM phase journal.
//! - Every host runs the pinned `outbe-up` binary extracted from `software-bundle.tar.zst` at
//!   `<state>/outbe-up/bin/outbe-up-<sha16>`, verified via `version --json` (`binary_sha256`)
//!   before any action without touching `$PATH`.
//! - Founder keys never hit host `/tmp`: an in-memory tar of `vm/validator-N` is piped over stdin
//!   to `outbe-up disk prepare --role founder --source - --bundle <operator-bundle> --staging-name <vm>`,
//!   reused across retries via `disk clean --list --json`, and consumed on `vm create --disk-staging <vm>
//!   --consume-staging` only after `Ready`.
//! - Zero host-config writes; `deploy` exits with code 2 when `outbe-up host check --json` reports
//!   missing host prerequisites (no wait loop, no DHCP reservation strategies).
//! - SSH ControlMaster multiplexes one connection per host (`0700` per-run socket dir, `ssh -O exit`
//!   on drop); guest JSON-RPC is batched over stdin to each VM's service IP (`bridge_ip:8545`).

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    progress::{Progress, WatchDisplay},
    rpc::ConsensusStatus,
};
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh, SshControlSession};
use outbe_software_bundle::{sha256_file, verify_and_extract, verify_archive};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::JoinSet;
use walkdir::WalkDir;

use super::{
    build::DeploymentKind,
    config::{FounderPublishedPorts, ResolvedFounderVm, VmFlavor},
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkMemberState, NetworkVmState},
};

pub const EXIT_CODE_HOST_CONFIG_REQUIRED: u8 = 2;
const SERIAL_LOG_TAIL_LINES: usize = 40;
const HOST_CHECK_EXIT_SENTINEL: &str = "__OUTBE_EXIT__=";
const READY_CONSECUTIVE_SAMPLES: u32 = 2;

/// Typed error carrying a specific process exit code (e.g. exit code 2 when `deploy` requires
/// manual host configuration before `network start`).
#[derive(Debug)]
pub struct ExitCodeError {
    pub code: u8,
    pub message: String,
}

impl fmt::Display for ExitCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitCodeError {}

#[derive(Clone, Debug)]
pub enum ImageSource {
    Archive {
        path: PathBuf,
        sha256: Option<String>,
    },
    Url {
        url: String,
        sha256: String,
    },
}

#[derive(Clone, Debug)]
pub struct DeployOptions<'a> {
    pub artifacts: &'a Path,
    pub image_source: ImageSource,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub no_tee: bool,
}

#[derive(Clone, Debug)]
pub struct StartOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
}

#[derive(Clone, Debug)]
pub struct StopOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub force: bool,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct StatusOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub watch: bool,
    pub max_lag: u64,
    pub json: bool,
}

#[derive(Clone, Debug)]
pub struct RemoveOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub force: bool,
    pub timeout: Duration,
}

// ---------------------------------------------------------------------------
// Strict remote `outbe-up --json` response schemas (required fields have NO defaults)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RemoteVersionOutput {
    name: String,
    version: String,
    binary_sha256: String,
}

#[derive(Debug, Deserialize)]
struct RemoteHostStatusOutput {
    ready_for_vm: bool,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteImagePublishOutput {
    image_name: String,
    os_image_hash: String,
    target_dir: String,
    #[allow(dead_code)]
    reused: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteListedImage {
    name: String,
    os_image_hash: String,
    path: String,
    has_archive_manifest: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteDiskPrepareOutput {
    #[allow(dead_code)]
    output: String,
    #[serde(default)]
    #[allow(dead_code)]
    staging_name: Option<String>,
    #[allow(dead_code)]
    size_gib: u32,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RemoteStagedDiskInfo {
    staging_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    path: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    size_bytes: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteCleanStagingOutput {
    #[allow(dead_code)]
    staging_name: String,
    #[allow(dead_code)]
    path: String,
    #[allow(dead_code)]
    removed: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteCleanStaleOutput {
    #[allow(dead_code)]
    staging_root: String,
    #[serde(default)]
    removed: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVmCreateOutput {
    name: String,
    vm_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    owner_tag: Option<String>,
    image_name: String,
    os_image_hash: String,
    mac: String,
    #[allow(dead_code)]
    bridge: String,
    #[allow(dead_code)]
    phase: String,
    #[serde(default)]
    staging_consumed: bool,
    #[serde(default)]
    warning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVmStartOutput {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    vm_id: String,
    #[allow(dead_code)]
    already_running: bool,
    #[allow(dead_code)]
    vmm_status: String,
}

#[derive(Debug, Deserialize)]
struct RemoteVmStopOutput {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    vm_id: String,
    #[allow(dead_code)]
    already_stopped: bool,
    #[allow(dead_code)]
    forced: bool,
    #[allow(dead_code)]
    vmm_status: String,
    #[serde(default)]
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RemoteVmStatusOutput {
    pub(crate) name: String,
    pub(crate) vm_id: String,
    #[serde(default)]
    pub(crate) owner_tag: Option<String>,
    pub(crate) phase: String,
    pub(crate) vmm_status: String,
    pub(crate) running: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) uptime: String,
    pub(crate) mac: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub(crate) service_ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub(crate) dhcp_ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub(crate) intended_ip: Option<Ipv4Addr>,
    pub(crate) image_name: String,
    pub(crate) os_image_hash: String,
    #[serde(default)]
    pub(crate) disk_source_sha256: Option<String>,
    #[allow(dead_code)]
    pub(crate) app_id: String,
    pub(crate) no_tee: bool,
    pub(crate) vcpu: u32,
    pub(crate) memory_gib: u32,
    pub(crate) disk_gib: u32,
    pub(crate) bridge: String,
    #[serde(default)]
    pub(crate) boot_error: Option<String>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteVmLogsOutput {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    vm_id: String,
    #[allow(dead_code)]
    lines: usize,
    log: String,
}

#[derive(Debug, Deserialize)]
struct RemoteVmRemoveOutput {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    vm_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    forgot_intent: bool,
    #[serde(default)]
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RemoteHostCheckOutput {
    pub(crate) vm: String,
    #[allow(dead_code)]
    pub(crate) vm_id: String,
    #[allow(dead_code)]
    pub(crate) mac: String,
    #[allow(dead_code)]
    pub(crate) ip: Ipv4Addr,
    pub(crate) ready: bool,
    pub(crate) host_network_ok: bool,
    pub(crate) service_ip_ok: bool,
    pub(crate) published_ports_ok: bool,
    #[serde(default)]
    pub(crate) missing: Vec<String>,
    #[serde(default)]
    pub(crate) conflicts: Vec<String>,
    #[serde(default)]
    pub(crate) recommended_forward_rules: Vec<String>,
    #[serde(default)]
    pub(crate) recommended_prerouting_rules: Vec<String>,
    #[serde(default)]
    pub(crate) recommended_firewall_commands: Vec<String>,
}

// ---------------------------------------------------------------------------
// Desired member specification and host-authoritative classification
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct DesiredMember {
    pub(crate) index: usize,
    pub(crate) role: MemberRole,
    pub(crate) name: String,
    pub(crate) vm_name: String,
    pub(crate) owner_tag: String,
    pub(crate) vmm_host: Ssh,
    pub(crate) vmm_home: String,
    pub(crate) bridge: String,
    pub(crate) service_ip: Ipv4Addr,
    pub(crate) advertise_ip: Ipv4Addr,
    pub(crate) port_offset: u16,
    pub(crate) vcpu: u32,
    pub(crate) memory_gib: u32,
    pub(crate) disk_gib: u32,
    pub(crate) flavor: VmFlavor,
    pub(crate) no_tee: bool,
    pub(crate) image_name: Option<String>,
    pub(crate) image_hash: Option<String>,
    pub(crate) recorded_disk_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VmClassKind {
    Absent,
    Foreign,
    Stale,
    Partial,
    Ours,
}

#[derive(Clone, Debug)]
pub(crate) enum VmClassification {
    Absent,
    Foreign {
        observed: Box<RemoteVmStatusOutput>,
        reason: String,
    },
    Stale {
        observed: Box<RemoteVmStatusOutput>,
        reasons: Vec<String>,
    },
    Partial {
        observed: Box<RemoteVmStatusOutput>,
    },
    Ours {
        observed: Box<RemoteVmStatusOutput>,
    },
}

impl VmClassification {
    pub(crate) fn kind(&self) -> VmClassKind {
        match self {
            Self::Absent => VmClassKind::Absent,
            Self::Foreign { .. } => VmClassKind::Foreign,
            Self::Stale { .. } => VmClassKind::Stale,
            Self::Partial { .. } => VmClassKind::Partial,
            Self::Ours { .. } => VmClassKind::Ours,
        }
    }
}

pub(crate) fn network_owner_tag_prefix(network_name: &str, genesis_sha256: &str) -> String {
    let prefix8 = &genesis_sha256[..genesis_sha256.len().min(8)];
    format!("outbe-net:{network_name}:{prefix8}:")
}

pub(crate) fn member_owner_tag(
    network_name: &str,
    genesis_sha256: &str,
    member_name: &str,
) -> String {
    format!(
        "{}{member_name}",
        network_owner_tag_prefix(network_name, genesis_sha256)
    )
}

pub(crate) fn founder_vm_name(network_name: &str, founder_name: &str) -> String {
    format!("{network_name}-{founder_name}")
}

/// Classify an observed host VM against a desired network member specification.
pub(crate) fn classify_host_vm(
    desired: &DesiredMember,
    observed: Option<&RemoteVmStatusOutput>,
) -> VmClassification {
    let Some(vm) = observed else {
        return VmClassification::Absent;
    };

    if vm.owner_tag.as_deref() != Some(desired.owner_tag.as_str()) {
        let reason = match vm.owner_tag.as_deref() {
            Some(other) => format!(
                "owner_tag mismatch: host VM `{}` has tag `{other}`, expected `{}`",
                vm.name, desired.owner_tag
            ),
            None => format!(
                "host VM `{}` has no owner_tag (unmanaged or pre-S6.3 VM), expected `{}`",
                vm.name, desired.owner_tag
            ),
        };
        return VmClassification::Foreign {
            observed: Box::new(vm.clone()),
            reason,
        };
    }

    let mut non_image_drifts = Vec::new();
    if vm.bridge != desired.bridge {
        non_image_drifts.push(format!("bridge (`{}` vs `{}`)", vm.bridge, desired.bridge));
    }
    let observed_service_ip = vm.service_ip.or(vm.intended_ip);
    if observed_service_ip != Some(desired.service_ip) {
        non_image_drifts.push(format!(
            "service_ip ({:?} vs {})",
            observed_service_ip, desired.service_ip
        ));
    }
    if vm.vcpu != desired.vcpu {
        non_image_drifts.push(format!("vcpu ({} vs {})", vm.vcpu, desired.vcpu));
    }
    if vm.memory_gib != desired.memory_gib {
        non_image_drifts.push(format!(
            "memory_gib ({} vs {})",
            vm.memory_gib, desired.memory_gib
        ));
    }
    if vm.disk_gib != desired.disk_gib {
        non_image_drifts.push(format!(
            "disk_gib ({} vs {})",
            vm.disk_gib, desired.disk_gib
        ));
    }
    if vm.no_tee != desired.no_tee {
        non_image_drifts.push(format!("no_tee ({} vs {})", vm.no_tee, desired.no_tee));
    }
    if let Some(expected_disk) = desired.recorded_disk_sha256.as_deref() {
        if vm.disk_source_sha256.as_deref() != Some(expected_disk) {
            non_image_drifts.push(format!(
                "disk_source_sha256 ({:?} vs `{expected_disk}`)",
                vm.disk_source_sha256
            ));
        }
    }

    let image_mismatch = desired
        .image_hash
        .as_deref()
        .is_some_and(|h| vm.os_image_hash != h)
        || desired
            .image_name
            .as_deref()
            .is_some_and(|img| vm.image_name != img);
    if image_mismatch {
        non_image_drifts.push(format!(
            "image (`{}`/{} vs {:?}/{:?})",
            vm.image_name, vm.os_image_hash, desired.image_name, desired.image_hash
        ));
    }

    if vm.phase != "ready" {
        if !non_image_drifts.is_empty() {
            return VmClassification::Stale {
                observed: Box::new(vm.clone()),
                reasons: non_image_drifts,
            };
        }
        return VmClassification::Partial {
            observed: Box::new(vm.clone()),
        };
    }

    if vm.vmm_status == "missing" {
        non_image_drifts.push("VMM reports VM missing".to_owned());
    }

    if !non_image_drifts.is_empty() {
        return VmClassification::Stale {
            observed: Box::new(vm.clone()),
            reasons: non_image_drifts,
        };
    }

    VmClassification::Ours {
        observed: Box::new(vm.clone()),
    }
}

// ---------------------------------------------------------------------------
// Public entry points (using SSH ControlMaster per-run session)
// ---------------------------------------------------------------------------

pub async fn deploy(options: DeployOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = deploy_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub async fn start(options: StartOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = start_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub async fn stop(options: StopOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = stop_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub async fn status(options: StatusOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = status_with_exec(&options, exec).await;
    session.close_all();
    res
}

pub async fn remove(options: RemoveOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = remove_with_exec(&options, exec).await;
    session.close_all();
    res
}

// ---------------------------------------------------------------------------
// Deploy (host-authoritative reconcile, pinned outbe-up, stdin tar, exit 2 on host check)
// ---------------------------------------------------------------------------

pub(crate) async fn deploy_with_exec(
    options: &DeployOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying VM network artifacts, bundles, and image source");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let local_op_bundle = build.root.join(&build.manifest.joining.bundle);
    ensure!(
        local_op_bundle.is_file(),
        "operator bundle missing at {}",
        local_op_bundle.display()
    );
    ensure!(
        sha256_file(&local_op_bundle)? == build.manifest.joining.bundle_sha256,
        "operator bundle changed after network build: {}",
        local_op_bundle.display()
    );

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let (resolved_source, image_source_id) = resolve_image_source(&options.image_source)?;

    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = match NetworkVmState::load_optional_locked(&state_lock, options.artifacts)? {
        Some(existing) => {
            existing.ensure_matches_network(&build)?;
            existing
        }
        None => NetworkVmState::new(&build, image_source_id.clone(), outbe_up_sha256.clone()),
    };
    state.outbe_up_sha256 = outbe_up_sha256.clone();

    let mut desired_members =
        build_desired_members(&build, Some(&state), Some(options.no_tee), None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);

    // 1. Pin & verify `outbe-up` by SHA-256 on every unique host, check `host status`,
    //    and sweep stale temp dirs (`disk clean --stale`).
    progress.stage(&format!(
        "pinning outbe-up ({}), checking host readiness, and cleaning stale staging on {} host(s)",
        &outbe_up_sha256[..16],
        unique_hosts.len()
    ));
    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            true,
        )?;
        let status_cmd = bin.cmd(&format!(
            "host status --bridge {} --vmm-home {} --json",
            shell_quote(&host.bridge),
            shell_quote_tilde_path(&host.vmm_home),
        ));
        let host_status: RemoteHostStatusOutput =
            run_host_json(exec.as_ref(), &host.ssh, &host.ssh.host, &status_cmd).wrap_err_with(
                || {
                    format!(
                        "inspect host readiness via `outbe-up host status` on {}",
                        host.ssh.host
                    )
                },
            )?;
        ensure!(
            host_status.ready_for_vm,
            "VMM host {} is not ready for VMs (`outbe-up host status` reported ready_for_vm=false): {}",
            host.ssh.host,
            if host_status.notes.is_empty() {
                format!("run `outbe-up host status --bridge {}` on {} for details", host.bridge, host.ssh.host)
            } else {
                host_status.notes.join("; ")
            }
        );

        let stale_cmd = bin.cmd("disk clean --stale --json");
        let stale: RemoteCleanStaleOutput =
            run_host_json(exec.as_ref(), &host.ssh, &host.ssh.host, &stale_cmd)
                .wrap_err_with(|| format!("clean stale staging dirs on {}", host.ssh.host))?;
        if !stale.removed.is_empty() {
            progress.update(&format!(
                "cleaned {} stale staging dir(s) on {}",
                stale.removed.len(),
                host.ssh.host
            ));
        }
        host_bins.insert(hkey.clone(), bin);
    }

    // 2. Distribute guest OS image once per unique host and verify uniform `(image_name, os_image_hash)`.
    progress.stage(&format!(
        "ensuring guest OS image on {} host(s)",
        unique_hosts.len()
    ));
    let mut host_images: BTreeMap<String, RemoteImagePublishOutput> = BTreeMap::new();
    let mut uniform_image: Option<(String, String)> = None;
    for (hkey, host) in &unique_hosts {
        let bin = &host_bins[hkey];
        let published = match &resolved_source {
            ResolvedImageSource::Archive { path, sha256 } => {
                import_image_on_host(exec.as_ref(), &host.ssh, bin, &host.vmm_home, path, sha256)?
            }
            ResolvedImageSource::Url { url, sha256 } => {
                fetch_image_on_host(exec.as_ref(), &host.ssh, bin, &host.vmm_home, url, sha256)?
            }
        };
        match &uniform_image {
            None => {
                uniform_image = Some((
                    published.image_name.clone(),
                    published.os_image_hash.clone(),
                ));
            }
            Some((img_name, img_hash)) => {
                ensure!(
                    *img_name == published.image_name && *img_hash == published.os_image_hash,
                    "image digest mismatch across hosts: host {} reported `{}` ({}), expected `{img_name}` ({img_hash})",
                    host.ssh.host,
                    published.image_name,
                    published.os_image_hash
                );
            }
        }
        host_images.insert(hkey.clone(), published);
    }
    let (image_name, image_hash) =
        uniform_image.ok_or_else(|| eyre::eyre!("no target hosts found for VM deployment"))?;

    // Refresh desired members with resolved guest image identity.
    for m in &mut desired_members {
        m.image_name = Some(image_name.clone());
        m.image_hash = Some(image_hash.clone());
    }
    state.image_source_id = image_source_id;
    state.image_name = image_name.clone();
    state.image_hash = image_hash.clone();

    // 3. Host-authoritative reconcile: fetch all VMs and staged disks from every unique host.
    progress.stage("reconciling host VM and staging state");
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);
    let mut host_vms: BTreeMap<String, Vec<RemoteVmStatusOutput>> = BTreeMap::new();
    let mut host_staged: BTreeMap<String, BTreeMap<String, RemoteStagedDiskInfo>> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let bin = &host_bins[hkey];
        let vms = fetch_host_vms(exec.as_ref(), &host.ssh, bin, &host.vmm_home)?;
        let staged = fetch_host_staged_disks(exec.as_ref(), &host.ssh, bin)?;
        let map = staged
            .into_iter()
            .map(|item| (item.staging_name.clone(), item))
            .collect();
        host_vms.insert(hkey.clone(), vms);
        host_staged.insert(hkey.clone(), map);
    }

    let mut expected_names: BTreeSet<&str> =
        desired_members.iter().map(|m| m.vm_name.as_str()).collect();
    for extra in state
        .members
        .iter()
        .filter(|m| m.role != MemberRole::Founder)
    {
        expected_names.insert(extra.vm_name.as_str());
    }
    let mut blockers = Vec::new();
    let mut plans: Vec<(&DesiredMember, VmClassification)> =
        Vec::with_capacity(desired_members.len());
    for member in &desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = host_vms[&hkey].iter().find(|vm| vm.name == member.vm_name);
        let class = classify_host_vm(member, observed);
        match &class {
            VmClassification::Foreign { observed, reason } => {
                blockers.push(format!(
                    "member `{}` (`{}` on {}, vm_id `{}`): FOREIGN ({reason}) — not ours: check the owner, and use another network name or VM name.",
                    member.name, member.vm_name, member.vmm_host.host, observed.vm_id
                ));
            }
            VmClassification::Stale { observed, reasons } => {
                let rm_hint = if observed.vm_id.is_empty() {
                    format!(
                        "outbe-up vm remove --name {} --forget-intent",
                        observed.name
                    )
                } else {
                    format!(
                        "outbe-up vm remove --name {} --expect-vm-id {}",
                        observed.name, observed.vm_id
                    )
                };
                blockers.push(format!(
                    "member `{}` (`{}` on {}): STALE ({}). Remove and redeploy: `{rm_hint}` (or `outbe-net-up network remove --artifacts {}`)",
                    member.name,
                    member.vm_name,
                    member.vmm_host.host,
                    reasons.join(", "),
                    build.root.display()
                ));
            }
            VmClassification::Partial { observed } if observed.phase == "removing" => {
                blockers.push(format!(
                    "member `{}` (`{}` on {}): PARTIAL (mid-removal). Finish removal with `outbe-up vm remove --name {}` before redeploying.",
                    member.name, member.vm_name, member.vmm_host.host, observed.name
                ));
            }
            VmClassification::Absent
            | VmClassification::Partial { .. }
            | VmClassification::Ours { .. } => {}
        }
        plans.push((member, class));
    }

    // Detect orphan VMs carrying our network's owner_tag prefix on any target host.
    for (hkey, vms) in &host_vms {
        let host = &unique_hosts[hkey];
        for vm in vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
                && !expected_names.contains(vm.name.as_str())
            {
                blockers.push(format!(
                    "host {} has ORPHAN VM `{}` ({}, tag `{}`). Remove it with `outbe-up vm remove --name {} --expect-vm-id {}` (or `outbe-net-up network remove --artifacts {}`).",
                    host.ssh.host,
                    vm.name,
                    vm.vm_id,
                    vm.owner_tag.as_deref().unwrap_or(""),
                    vm.name,
                    vm.vm_id,
                    build.root.display()
                ));
            }
        }
    }

    ensure!(
        blockers.is_empty(),
        "refusing to deploy due to host VM conflicts:\n  - {}",
        blockers.join("\n  - ")
    );

    // 4. Determine which hosts need `disk prepare` and upload operator bundle ONCE per host.
    let mut hosts_needing_op_bundle: BTreeSet<String> = BTreeSet::new();
    for (member, class) in &plans {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        match class {
            VmClassification::Ours { .. } => {}
            VmClassification::Absent => {
                let reusable = can_reuse_staged_disk(
                    host_staged[&hkey].get(&member.vm_name),
                    member.recorded_disk_sha256.as_deref(),
                );
                if !reusable {
                    hosts_needing_op_bundle.insert(hkey);
                }
            }
            VmClassification::Partial { observed } => {
                let required_sha = observed
                    .disk_source_sha256
                    .as_deref()
                    .or(member.recorded_disk_sha256.as_deref());
                let reusable =
                    can_reuse_staged_disk(host_staged[&hkey].get(&member.vm_name), required_sha);
                if !reusable {
                    if member.role != MemberRole::Founder && required_sha.is_some() {
                        bail!(
                            "member `{}` (`{}` on {}) is in partial phase `{}` requiring staged disk sha256 `{}`, which is no longer present on host; remove the partial VM with `outbe-up vm remove --name {}` and re-run deploy",
                            member.name,
                            member.vm_name,
                            member.vmm_host.host,
                            observed.phase,
                            required_sha.unwrap_or(""),
                            member.vm_name
                        );
                    }
                    hosts_needing_op_bundle.insert(hkey);
                }
            }
            VmClassification::Foreign { .. } | VmClassification::Stale { .. } => {
                unreachable!("blocked above")
            }
        }
    }

    let mut uploaded_op_bundles: BTreeMap<String, (Ssh, String, String)> = BTreeMap::new();
    let op_bundle_filename = local_op_bundle
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("outbe-validator-operator-bundle.tar.zst");

    let deploy_body: Result<()> = (|| {
        for hkey in &hosts_needing_op_bundle {
            let host = &unique_hosts[hkey];
            let remote_dir =
                make_remote_temp_dir(exec.as_ref(), &host.ssh, "outbe-net-up-opbundle")?;
            exec.copy_to(
                &host.ssh,
                &host.ssh.host,
                &[local_op_bundle.as_path()],
                &remote_dir,
                false,
            )
            .wrap_err_with(|| format!("upload operator bundle to {}", host.ssh.host))?;
            let remote_bundle = format!("{remote_dir}/{op_bundle_filename}");
            uploaded_op_bundles.insert(hkey.clone(), (host.ssh.clone(), remote_dir, remote_bundle));
        }

        for (member, class) in &plans {
            let hkey = host_key(&member.vmm_host, &member.vmm_home);
            let bin = &host_bins[&hkey];
            let host_img = &host_images[&hkey];

            if let VmClassification::Ours { observed } = class {
                progress.update(&format!(
                    "reconciled `{}` ({}) on {}: already up-to-date",
                    member.vm_name, observed.vm_id, member.vmm_host.host
                ));
                state.upsert_member(build_member_state(
                    member,
                    &image_hash,
                    observed
                        .disk_source_sha256
                        .clone()
                        .or_else(|| member.recorded_disk_sha256.clone()),
                    Some(observed.vm_id.clone()),
                    Some(observed.mac.clone()),
                ));
                state.save_locked(&state_lock, options.artifacts)?;
                continue;
            }

            let partial_required_sha = match class {
                VmClassification::Partial { observed } => observed.disk_source_sha256.as_deref(),
                _ => None,
            };
            let reusable_sha = partial_required_sha.or(member.recorded_disk_sha256.as_deref());
            let disk_sha256 = if can_reuse_staged_disk(
                host_staged[&hkey].get(&member.vm_name),
                reusable_sha,
            ) {
                let sha = reusable_sha.expect("reusable_sha checked").to_owned();
                progress.update(&format!(
                    "reusing staged disk `{}` (sha256 {}...) on {}",
                    member.vm_name,
                    &sha[..12],
                    member.vmm_host.host
                ));
                sha
            } else {
                // Stream founder tree tar directly over stdin to `outbe-up disk prepare --source - --staging-name`.
                progress.stage(&format!(
                    "preparing founder data disk for {} on {} via stdin tar",
                    member.name, member.vmm_host.host
                ));
                let local_tree = build.root.join(format!("vm/validator-{}", member.index));
                let tar_bytes = build_founder_tar_bytes(&local_tree)?;
                let (_, _, remote_op_bundle) = &uploaded_op_bundles[&hkey];
                let prep_cmd = bin.cmd(&format!(
                    "disk prepare --role founder --source - --bundle {} --staging-name {} --size-gib {} --json",
                    shell_quote(remote_op_bundle),
                    shell_quote(&member.vm_name),
                    member.disk_gib,
                ));
                let raw = exec
                    .capture_stdin(&member.vmm_host, &member.name, &prep_cmd, &tar_bytes)
                    .wrap_err_with(|| {
                        format!(
                            "stream founder data disk for `{}` on {}",
                            member.vm_name, member.vmm_host.host
                        )
                    })?;
                let prep: RemoteDiskPrepareOutput =
                    serde_json::from_str(&raw).wrap_err_with(|| {
                        format!(
                            "parse `outbe-up disk prepare --json` output for `{}`",
                            member.vm_name
                        )
                    })?;
                if let Some(expected_sha) = partial_required_sha {
                    ensure!(
                        prep.sha256 == expected_sha,
                        "re-prepared staged disk for partial VM `{}` produced sha256 `{}`, which differs from recorded `{expected_sha}`; remove the partial VM with `outbe-up vm remove --name {}` and re-run deploy",
                        member.vm_name,
                        prep.sha256,
                        member.vm_name
                    );
                }
                // Record prepared disk_source_sha256 immediately (amendment 3).
                state.upsert_member(build_member_state(
                    member,
                    &image_hash,
                    Some(prep.sha256.clone()),
                    state.member(&member.vm_name).and_then(|m| m.vm_id.clone()),
                    state.member(&member.vm_name).and_then(|m| m.mac.clone()),
                ));
                state.save_locked(&state_lock, options.artifacts)?;
                prep.sha256
            };

            // Create (or resume partial) stopped VM via `--disk-staging <vm> --consume-staging`.
            progress.stage(&format!(
                "creating stopped VM `{}` on {}",
                member.vm_name, member.vmm_host.host
            ));
            let flavor_arg = match member.flavor {
                VmFlavor::Dev => "dev",
                VmFlavor::Prod => "prod",
            };
            let mut create_inner = format!(
                "vm create --name {} --owner-tag {} --image {} --image-name {} --flavor {} --disk-staging {} --consume-staging --bridge {} --ip {} --vcpu {} --memory-gib {} --disk-gib {} --vmm-home {}",
                shell_quote(&member.vm_name),
                shell_quote(&member.owner_tag),
                shell_quote(&host_img.target_dir),
                shell_quote(&host_img.image_name),
                flavor_arg,
                shell_quote(&member.vm_name),
                shell_quote(&member.bridge),
                member.service_ip,
                member.vcpu,
                member.memory_gib,
                member.disk_gib,
                shell_quote_tilde_path(&member.vmm_home),
            );
            if member.no_tee {
                create_inner.push_str(" --no-tee");
            }
            create_inner.push_str(" --json");
            let create_cmd = bin.cmd(&create_inner);
            let created: RemoteVmCreateOutput =
                run_host_json(exec.as_ref(), &member.vmm_host, &member.name, &create_cmd)
                    .wrap_err_with(|| {
                        format!("create VM `{}` on {}", member.vm_name, member.vmm_host.host)
                    })?;
            ensure!(
                created.name == member.vm_name
                    && created.image_name == image_name
                    && created.os_image_hash == image_hash,
                "unexpected `vm create` result for `{}` on {}",
                member.vm_name,
                member.vmm_host.host
            );
            if let Some(warning) = &created.warning {
                eprintln!("{warning}");
            }
            if !created.staging_consumed {
                let clean_cmd = bin.cmd(&format!(
                    "disk clean --staging-name {} --json",
                    shell_quote(&member.vm_name)
                ));
                let _ = run_host_json::<dyn HostExec, RemoteCleanStagingOutput>(
                    exec.as_ref(),
                    &member.vmm_host,
                    &member.name,
                    &clean_cmd,
                );
            }

            state.upsert_member(build_member_state(
                member,
                &image_hash,
                Some(disk_sha256),
                Some(created.vm_id),
                Some(created.mac),
            ));
            state.save_locked(&state_lock, options.artifacts)?;
        }
        Ok(())
    })();

    // Always clean up uploaded operator bundle temp dirs on every host.
    for (ssh, remote_dir, _) in uploaded_op_bundles.values() {
        let _ = cleanup_remote_dir(exec.as_ref(), ssh, remote_dir);
    }
    deploy_body?;

    // 5. Mandatory read-only `host check` gate (zero writes, no wait loop; exit code 2 if not ready).
    progress.stage("verifying host networking and published ports (`outbe-up host check`)");
    let publish_ctx = PublishContext::from_members(&desired_members);
    let checks = run_host_checks(&desired_members, &host_bins, exec.as_ref(), &publish_ctx)?;
    let all_ready = checks.iter().all(|(_, c)| c.ready);
    if all_ready {
        progress.done(&format!(
            "deployed {} stopped VM(s); host checks passed",
            desired_members.len()
        ));
        println!(
            "Next: outbe-net-up network start --artifacts {}",
            options.artifacts.display()
        );
        return Ok(());
    }

    let report_text = format_failed_host_checks(&checks);
    println!(
        "\nHost configuration required before starting the network (read-only; zero writes performed):\n{report_text}\n\nApply the host configuration above, then run:\n  outbe-net-up network start --artifacts {}",
        options.artifacts.display()
    );
    Err(eyre::Report::new(ExitCodeError {
        code: EXIT_CODE_HOST_CONFIG_REQUIRED,
        message: format!(
            "host check failed on one or more hosts; apply the host configuration printed above, then run `outbe-net-up network start --artifacts {}`",
            options.artifacts.display()
        ),
    }))
}

// ---------------------------------------------------------------------------
// Start (host-authoritative reconcile + host check gate + parallel start + 2-sample readiness)
// ---------------------------------------------------------------------------

pub(crate) async fn start_with_exec(
    options: &StartOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading network artifacts and reconciling hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let mut state = NetworkVmState::load_optional_locked(&state_lock, options.artifacts)?;
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }

    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);

    // 1. Verify pinned `outbe-up` on every unique host.
    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            true,
        )?;
        host_bins.insert(hkey.clone(), bin);
    }

    // 2. Host-authoritative reconcile: every member MUST be `Ours` with a known `vm_id`.
    let mut live_targets: Vec<(DesiredMember, RemoteVmStatusOutput)> =
        Vec::with_capacity(desired_members.len());
    let mut problems = Vec::new();
    let mut host_vms_cache: BTreeMap<String, Vec<RemoteVmStatusOutput>> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let vms = fetch_host_vms(exec.as_ref(), &host.ssh, &host_bins[hkey], &host.vmm_home)?;
        host_vms_cache.insert(hkey.clone(), vms);
    }

    for member in &desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = host_vms_cache[&hkey]
            .iter()
            .find(|vm| vm.name == member.vm_name);
        match classify_host_vm(member, observed) {
            VmClassification::Ours { observed } => {
                if let Some(st) = state.as_mut() {
                    if let Some(m) = st.members.iter_mut().find(|m| m.vm_name == member.vm_name) {
                        m.vm_id = Some(observed.vm_id.clone());
                        m.mac = Some(observed.mac.clone());
                    }
                }
                live_targets.push((member.clone(), *observed));
            }
            VmClassification::Absent => problems.push(format!(
                "{} (`{}` on {}): absent (run `outbe-net-up network deploy --artifacts {}`)",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                options.artifacts.display()
            )),
            VmClassification::Partial { observed } => problems.push(format!(
                "{} (`{}` on {}): partial phase `{}` (resume `network deploy`)",
                member.name, member.vm_name, member.vmm_host.host, observed.phase
            )),
            VmClassification::Foreign { reason, .. } => {
                problems.push(format!(
                    "{} (`{}`): foreign ({reason})",
                    member.name, member.vm_name
                ));
            }
            VmClassification::Stale { reasons, .. } => problems.push(format!(
                "{} (`{}`): stale ({})",
                member.name,
                member.vm_name,
                reasons.join(", ")
            )),
        }
    }
    ensure!(
        problems.is_empty(),
        "cannot start VM network: host reconciliation found non-ready members:\n  - {}",
        problems.join("\n  - ")
    );
    if let Some(st) = &state {
        let _ = st.save_locked(&state_lock, options.artifacts);
    }
    drop(state_lock);

    // 3. Mandatory `host check` gate BEFORE starting any VM.
    progress.stage("verifying host networking and firewall rules (`outbe-up host check`)");
    let publish_ctx = PublishContext::from_members(&desired_members);
    let checks = run_host_checks(&desired_members, &host_bins, exec.as_ref(), &publish_ctx)?;
    let failed_checks: Vec<_> = checks.iter().filter(|(_, c)| !c.ready).collect();
    if !failed_checks.is_empty() {
        let details = format_failed_host_checks(&checks);
        bail!(
            "refusing to start VM network: `outbe-up host check` failed for {}/{} VM(s). Apply the recommended host configuration and re-run `outbe-net-up network start`:\n{details}",
            failed_checks.len(),
            checks.len()
        );
    }

    // 4. Start all VMs in parallel with `--expect-vm-id`.
    progress.stage(&format!(
        "starting {} founding validator VM(s) in parallel",
        live_targets.len()
    ));
    let mut start_tasks = JoinSet::new();
    for (member, vm) in &live_targets {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = host_bins[&hkey].clone();
        let exec = Arc::clone(&exec);
        let member = member.clone();
        let vm_id = vm.vm_id.clone();
        start_tasks.spawn_blocking(move || {
            let cmd = bin.cmd(&format!(
                "vm start --name {} --expect-vm-id {} --vmm-home {} --json",
                shell_quote(&member.vm_name),
                shell_quote(&vm_id),
                shell_quote_tilde_path(&member.vmm_home),
            ));
            let _: RemoteVmStartOutput =
                run_host_json(exec.as_ref(), &member.vmm_host, &member.name, &cmd).wrap_err_with(
                    || {
                        format!(
                            "start VM `{}` ({vm_id}) on {}",
                            member.vm_name, member.vmm_host.host
                        )
                    },
                )?;
            Ok::<(), eyre::Report>(())
        });
    }
    while let Some(res) = start_tasks.join_next().await {
        res.wrap_err("VM start task panicked")??;
    }

    // 5. Poll batched JSON-RPC over SSH ControlMaster until readiness holds in TWO consecutive
    //    samples with `min(finalized) >= B + min_finalized_blocks` (amendment 5).
    progress.stage(&format!(
        "waiting for finalized consensus growth (+{} blocks across {} consecutive samples)",
        options.min_finalized_blocks, READY_CONSECUTIVE_SAMPLES
    ));
    let deadline = Instant::now() + options.timeout;
    let required_peers = live_targets
        .iter()
        .filter(|(m, _)| m.role != MemberRole::FullNode)
        .count()
        .saturating_sub(1);
    let mut baseline_b: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let (statuses, _) =
            query_members_and_orphans(&build, &desired_members, &unique_hosts, &host_bins, &exec)
                .await?;
        let assessments = assess_members_lag(&statuses, options.max_lag);

        // B = max(finalized) on the FIRST sample after vm start where all nodes report a finalized
        // height, so every node must reach >= max_at_start + N (proving actual chain growth rather
        // than a lagging node catching up to a halted chain).
        if baseline_b.is_none() {
            baseline_b = compute_readiness_baseline(&statuses);
        }

        let sample_ready = evaluate_network_readiness(
            &statuses,
            &assessments,
            build.config.chain_id,
            required_peers,
            baseline_b,
            options.min_finalized_blocks,
        );

        if sample_ready {
            consecutive_ready += 1;
            if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
                progress.done(&format!(
                    "VM network {} finalized (baseline B={}, growth >= +{}, {} consecutive ready samples)",
                    build.config.name,
                    baseline_b.unwrap_or(0),
                    options.min_finalized_blocks,
                    consecutive_ready
                ));
                println!(
                    "\n{}",
                    render_vm_status(&build, &statuses, &[], options.max_lag)
                );
                return Ok(());
            }
        } else {
            consecutive_ready = 0;
        }

        if Instant::now() >= deadline {
            bail!(
                "{}",
                build_timeout_report(
                    &build,
                    &live_targets,
                    &host_bins,
                    exec.as_ref(),
                    options.timeout,
                    &statuses,
                    options.max_lag,
                    baseline_b,
                    options.min_finalized_blocks,
                )
            );
        }

        let min_fin = statuses
            .iter()
            .filter_map(|s| s.finalized)
            .min()
            .map(|h| h.to_string())
            .unwrap_or_else(|| "-".to_owned());
        progress.update(&format!(
            "baseline={} min_finalized={min_fin} target_growth=+{} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            baseline_b.map(|b| b.to_string()).unwrap_or_else(|| "-".to_owned()),
            options.min_finalized_blocks,
        ));

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            continue;
        }
        tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
    }
}

// ---------------------------------------------------------------------------
// Stop (host-authoritative, parallel with `--expect-vm-id`, graceful by default)
// ---------------------------------------------------------------------------

pub(crate) async fn stop_with_exec(
    options: &StopOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying pinned outbe-up and discovering network VMs on hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let state = {
        let lock = NetworkVmState::lock(options.artifacts)?;
        NetworkVmState::load_optional_locked(&lock, options.artifacts)?
    };
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }
    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);

    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    let mut stoppable: Vec<(Ssh, String, RemoteOutbeUp, RemoteVmStatusOutput)> = Vec::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            true,
        )?;
        let vms = fetch_host_vms(exec.as_ref(), &host.ssh, &bin, &host.vmm_home)?;
        for vm in vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
                && !vm.vm_id.is_empty()
                && vm.phase != "creating"
            {
                stoppable.push((host.ssh.clone(), host.vmm_home.clone(), bin.clone(), vm));
            }
        }
        host_bins.insert(hkey.clone(), bin);
    }

    let mode_label = if options.force {
        "force-stopping"
    } else {
        "gracefully stopping"
    };
    progress.stage(&format!(
        "{mode_label} {} network VM(s) in parallel",
        stoppable.len()
    ));

    let mut tasks = JoinSet::new();
    for (ssh, vmm_home, bin, vm) in stoppable {
        let exec = Arc::clone(&exec);
        let force = options.force;
        let timeout_secs = options.timeout.as_secs();
        tasks.spawn_blocking(move || {
            let mut inner = format!(
                "vm stop --name {} --expect-vm-id {} --timeout {} --vmm-home {}",
                shell_quote(&vm.name),
                shell_quote(&vm.vm_id),
                timeout_secs,
                shell_quote_tilde_path(&vmm_home),
            );
            if force {
                inner.push_str(" --force");
            }
            inner.push_str(" --json");
            let cmd = bin.cmd(&inner);
            let out: RemoteVmStopOutput = run_host_json(exec.as_ref(), &ssh, &vm.name, &cmd)
                .wrap_err_with(|| {
                    format!("stop VM `{}` ({}) on {}", vm.name, vm.vm_id, ssh.host)
                })?;
            Ok::<Option<String>, eyre::Report>(out.warning)
        });
    }

    let mut stopped = 0usize;
    while let Some(result) = tasks.join_next().await {
        if let Some(warning) = result.wrap_err("VM stop task panicked")?? {
            eprintln!("{warning}");
        }
        stopped += 1;
    }
    let _ = host_bins;

    progress.done(&format!(
        "{} {stopped} network VM(s)",
        if options.force {
            "force-stopped"
        } else {
            "gracefully stopped"
        }
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Status (works on full OR partial deployment; host-authoritative; table or `--json`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub(crate) struct VmMemberStatus {
    pub(crate) name: String,
    pub(crate) role: MemberRole,
    pub(crate) vm_name: String,
    pub(crate) vm_id: String,
    pub(crate) host: String,
    pub(crate) classification: VmClassKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) classification_detail: Option<String>,
    pub(crate) service_ip: Ipv4Addr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dhcp_ip: Option<Ipv4Addr>,
    pub(crate) mac: String,
    pub(crate) vmm_status: String,
    pub(crate) vmm_running: bool,
    pub(crate) no_tee: bool,
    pub(crate) chain_id: Option<u64>,
    pub(crate) finalized: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finalized_hash: Option<String>,
    pub(crate) execution_peers: Option<u64>,
    pub(crate) consensus_mode: Option<String>,
    pub(crate) consensus_peers: Option<u64>,
    pub(crate) has_threshold_shares: Option<bool>,
    pub(crate) vrf_material_version: Option<u64>,
    pub(crate) tee_ready: Option<bool>,
    #[serde(skip_serializing)]
    pub(crate) consensus: Option<ConsensusStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) boot_warning: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OrphanVmStatus {
    pub(crate) host: String,
    pub(crate) vm_name: String,
    pub(crate) vm_id: String,
    pub(crate) owner_tag: String,
    pub(crate) vmm_status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct MemberLagAssessment {
    pub(crate) lag: Option<u64>,
    pub(crate) lagging: bool,
    pub(crate) reasons: Vec<String>,
}

#[derive(Serialize)]
struct VmMemberStatusJson<'a> {
    #[serde(flatten)]
    status: &'a VmMemberStatus,
    lag: Option<u64>,
    lagging: bool,
    lag_reasons: &'a [String],
}

fn is_empty_slice<T>(slice: &[T]) -> bool {
    slice.is_empty()
}

#[derive(Serialize)]
struct VmNetworkStatusJson<'a> {
    network: &'a str,
    chain_id: u64,
    max_lag: u64,
    healthy_count: usize,
    total_count: usize,
    members: Vec<VmMemberStatusJson<'a>>,
    #[serde(skip_serializing_if = "is_empty_slice")]
    orphans: &'a [OrphanVmStatus],
}

pub(crate) async fn status_with_exec(
    options: &StatusOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let state = {
        let lock = NetworkVmState::lock(options.artifacts)?;
        NetworkVmState::load_optional_locked(&lock, options.artifacts)?
    };
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }

    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            false,
        )?;
        host_bins.insert(hkey.clone(), bin);
    }

    let (statuses, orphans) =
        query_members_and_orphans(&build, &desired_members, &unique_hosts, &host_bins, &exec)
            .await?;
    let assessments = assess_members_lag(&statuses, options.max_lag);

    if options.json {
        let required_peers = desired_members
            .iter()
            .filter(|m| m.role != MemberRole::FullNode)
            .count()
            .saturating_sub(1);
        let healthy_count = statuses
            .iter()
            .zip(&assessments)
            .filter(|(s, a)| {
                s.classification == VmClassKind::Ours
                    && member_node_ready(s, build.config.chain_id, required_peers)
                    && !a.lagging
            })
            .count();
        let members = statuses
            .iter()
            .zip(&assessments)
            .map(|(s, a)| VmMemberStatusJson {
                status: s,
                lag: a.lag,
                lagging: a.lagging,
                lag_reasons: &a.reasons,
            })
            .collect();
        let payload = VmNetworkStatusJson {
            network: &build.config.name,
            chain_id: build.config.chain_id,
            max_lag: options.max_lag,
            healthy_count,
            total_count: statuses.len(),
            members,
            orphans: &orphans,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return check_unhealthy_members(&statuses, &assessments, &orphans);
    }

    if !options.watch {
        let mut progress = Progress::new();
        progress.done(&format!("queried {} VM member(s)", statuses.len()));
        println!(
            "\n{}",
            render_vm_status(&build, &statuses, &orphans, options.max_lag)
        );
        return check_unhealthy_members(&statuses, &assessments, &orphans);
    }

    let mut progress = Progress::new();
    progress.done(&format!("connected to {} VM member(s)", statuses.len()));
    let display = WatchDisplay::new();
    display.render(&render_vm_status(
        &build,
        &statuses,
        &orphans,
        options.max_lag,
    ));
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        tokio::select! {
            result = &mut interrupt => {
                result.wrap_err("failed to listen for Ctrl-C")?;
                return Ok(());
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let (next, next_orphans) =
            query_members_and_orphans(&build, &desired_members, &unique_hosts, &host_bins, &exec)
                .await?;
        display.render(&render_vm_status(
            &build,
            &next,
            &next_orphans,
            options.max_lag,
        ));
    }
}

fn check_unhealthy_members(
    statuses: &[VmMemberStatus],
    assessments: &[MemberLagAssessment],
    orphans: &[OrphanVmStatus],
) -> Result<()> {
    let mut unhealthy = Vec::new();
    for (status, assessment) in statuses.iter().zip(assessments) {
        if status.classification != VmClassKind::Ours {
            unhealthy.push(format!("{}: {:?}", status.name, status.classification));
        } else if !status.vmm_running || status.error.is_some() || status.finalized.is_none() {
            unhealthy.push(format!("{}: unreachable/not-running", status.name));
        } else if assessment.lagging {
            unhealthy.push(format!(
                "{}: {}",
                status.name,
                assessment.reasons.join(", ")
            ));
        }
    }
    for orphan in orphans {
        unhealthy.push(format!("orphan `{}` on {}", orphan.vm_name, orphan.host));
    }
    if !unhealthy.is_empty() {
        bail!(
            "VM network has unhealthy, partial, or lagging member(s): {}",
            unhealthy.join("; ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Remove (host-authoritative by owner_tag + vm_id; cleans staging; verifies 0 VMs & 0 staging)
// ---------------------------------------------------------------------------

pub(crate) async fn remove_with_exec(
    options: &RemoveOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying pinned outbe-up and discovering network VMs on hosts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    let outbe_up_sha256 = resolve_bundled_outbe_up_sha256(&build.root)?;
    let state_lock = NetworkVmState::lock(options.artifacts)?;
    let state = NetworkVmState::load_optional_locked(&state_lock, options.artifacts)?;
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }

    let desired_members = build_all_desired_members(&build, state.as_ref(), None, None, None)?;
    let unique_hosts = collect_unique_hosts(&desired_members);
    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);

    let mut host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    let mut to_remove: Vec<(Ssh, String, RemoteOutbeUp, RemoteVmStatusOutput)> = Vec::new();
    for (hkey, host) in &unique_hosts {
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            true,
        )?;
        let vms = fetch_host_vms(exec.as_ref(), &host.ssh, &bin, &host.vmm_home)?;
        for vm in vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
            {
                to_remove.push((host.ssh.clone(), host.vmm_home.clone(), bin.clone(), vm));
            }
        }
        host_bins.insert(hkey.clone(), bin);
    }

    progress.stage(&format!(
        "removing {} tagged network VM(s) (graceful={}, leaving host config and guest images untouched)",
        to_remove.len(),
        !options.force
    ));

    let mut tasks = JoinSet::new();
    for (ssh, vmm_home, bin, vm) in to_remove.clone() {
        let exec = Arc::clone(&exec);
        let force = options.force;
        let timeout_secs = options.timeout.as_secs();
        tasks.spawn_blocking(move || {
            let mut inner = format!(
                "vm remove --name {} --timeout {} --vmm-home {}",
                shell_quote(&vm.name),
                timeout_secs,
                shell_quote_tilde_path(&vmm_home),
            );
            // Pass `--expect-vm-id` whenever the VM has a recorded UUID; omit for `creating` with
            // empty `vm_id` so `outbe-up` can run token-based recovery cleanly.
            if !vm.vm_id.is_empty() {
                inner.push_str(&format!(" --expect-vm-id {}", shell_quote(&vm.vm_id)));
            }
            if force {
                inner.push_str(" --force");
            }
            inner.push_str(" --json");
            let cmd = bin.cmd(&inner);
            let res: Result<RemoteVmRemoveOutput> =
                run_host_json(exec.as_ref(), &ssh, &vm.name, &cmd).wrap_err_with(|| {
                    format!("remove VM `{}` ({}) on {}", vm.name, vm.vm_id, ssh.host)
                });
            (vm.name, res)
        });
    }

    let mut remove_errors: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        let (vm_name, res) = joined.wrap_err("VM remove task panicked")?;
        match res {
            Ok(out) => {
                if let Some(warning) = out.warning {
                    eprintln!("{warning}");
                }
            }
            Err(err) => remove_errors.push(format!("{vm_name}: {err:#}")),
        }
    }

    // Amendment 2: clean staged disk for EVERY expected member even if no VM existed, plus
    // `disk clean --stale` on every unique host.
    progress.stage("cleaning staged raw disks and stale temporary directories on hosts");
    for member in &desired_members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &host_bins[&hkey];
        let cmd = bin.cmd(&format!(
            "disk clean --staging-name {} --json",
            shell_quote(&member.vm_name)
        ));
        if let Err(err) = run_host_json::<dyn HostExec, RemoteCleanStagingOutput>(
            exec.as_ref(),
            &member.vmm_host,
            &member.name,
            &cmd,
        ) {
            remove_errors.push(format!("disk clean {}: {err:#}", member.vm_name));
        }
    }
    for (hkey, host) in &unique_hosts {
        let bin = &host_bins[hkey];
        let cmd = bin.cmd("disk clean --stale --json");
        if let Err(err) = run_host_json::<dyn HostExec, RemoteCleanStaleOutput>(
            exec.as_ref(),
            &host.ssh,
            &host.ssh.host,
            &cmd,
        ) {
            remove_errors.push(format!("disk clean --stale on {}: {err:#}", host.ssh.host));
        }
    }

    if !remove_errors.is_empty() {
        bail!(
            "partial network removal (keeping {} for retry): {}",
            NetworkVmState::path(options.artifacts).display(),
            remove_errors.join("; ")
        );
    }

    // Amendment 2: verify ZERO tagged VMs AND ZERO staged disks for our members remain on ALL
    // hosts BEFORE deleting `network-vm-state.json`.
    let member_vm_names: BTreeSet<&str> =
        desired_members.iter().map(|m| m.vm_name.as_str()).collect();
    let mut leftovers = Vec::new();
    for (hkey, host) in &unique_hosts {
        let bin = &host_bins[hkey];
        let remaining_vms = fetch_host_vms(exec.as_ref(), &host.ssh, bin, &host.vmm_home)?;
        for vm in remaining_vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
            {
                leftovers.push(format!(
                    "VM `{}` ({}) still present on {}",
                    vm.name, vm.vm_id, host.ssh.host
                ));
            }
        }
        let remaining_staged = fetch_host_staged_disks(exec.as_ref(), &host.ssh, bin)?;
        for staged in remaining_staged {
            if member_vm_names.contains(staged.staging_name.as_str()) {
                leftovers.push(format!(
                    "staged disk `{}` still present on {}",
                    staged.staging_name, host.ssh.host
                ));
            }
        }
    }
    ensure!(
        leftovers.is_empty(),
        "refusing to delete {}: host verification still found remaining network resources: {}",
        NetworkVmState::path(options.artifacts).display(),
        leftovers.join("; ")
    );

    let deleted_state = NetworkVmState::delete_locked(&state_lock, options.artifacts)?;
    progress.done(&format!(
        "removed {} network VM(s), cleaned staging, {} (host config and guest images untouched)",
        to_remove.len(),
        if deleted_state {
            format!(
                "deleted {}",
                NetworkVmState::path(options.artifacts).display()
            )
        } else {
            "no local state file present".to_owned()
        }
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Pinned `outbe-up` resolution, installation, and command builder
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RemoteOutbeUp {
    bin_expr: String,
}

impl RemoteOutbeUp {
    fn cmd(&self, subcmd: &str) -> String {
        format!("{} {subcmd}", self.bin_expr)
    }
}

fn resolve_bundled_outbe_up_sha256(artifacts: &Path) -> Result<String> {
    let bundle_path = artifacts.join("software-bundle.tar.zst");
    let manifest = verify_archive(&bundle_path).wrap_err_with(|| {
        format!(
            "verify software bundle manifest at {}",
            bundle_path.display()
        )
    })?;
    let entry = manifest
        .files
        .iter()
        .find(|f| f.path == "bin/outbe-up")
        .ok_or_else(|| {
            eyre::eyre!(
                "software bundle {} does not contain `bin/outbe-up`",
                bundle_path.display()
            )
        })?;
    Ok(entry.sha256.clone())
}

fn extract_bundled_outbe_up_bytes(artifacts: &Path, expected_sha256: &str) -> Result<Vec<u8>> {
    let bundle_path = artifacts.join("software-bundle.tar.zst");
    let tmp = tempfile::Builder::new()
        .prefix("outbe-up-extract-")
        .tempdir()
        .wrap_err("create temp dir for outbe-up extraction")?;
    verify_and_extract(&bundle_path, tmp.path())?;
    let bin_path = tmp.path().join("bin/outbe-up");
    let actual_sha = sha256_file(&bin_path)?;
    ensure!(
        actual_sha == expected_sha256,
        "extracted bin/outbe-up digest mismatch: expected {expected_sha256}, got {actual_sha}"
    );
    fs::read(&bin_path).wrap_err("read extracted bin/outbe-up")
}

fn remote_outbe_up_paths(sha256: &str) -> (String, String, RemoteOutbeUp) {
    let sha16 = &sha256[..sha256.len().min(16)];
    let dir_expr = "\"${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up/bin\"".to_owned();
    let bin_expr =
        format!("\"${{XDG_STATE_HOME:-$HOME/.local/state}}/outbe-up/bin/outbe-up-{sha16}\"");
    (dir_expr, bin_expr.clone(), RemoteOutbeUp { bin_expr })
}

fn ensure_pinned_outbe_up_on_host(
    exec: &dyn HostExec,
    ssh: &Ssh,
    artifacts: &Path,
    expected_sha256: &str,
    allow_install: bool,
) -> Result<RemoteOutbeUp> {
    let (dir_expr, bin_expr, handle) = remote_outbe_up_paths(expected_sha256);
    let version_cmd = handle.cmd("version --json");

    // An existing binary under our 0700 state directory (`<state>/outbe-up/bin/outbe-up-<sha16>`)
    // is verified via its own `version --json` (`binary_sha256`) rather than an extra host
    // `sha256sum` invocation; this is acceptable under our threat model (the host owner already
    // controls the host).
    if let Ok(ver) =
        run_host_json::<dyn HostExec, RemoteVersionOutput>(exec, ssh, &ssh.host, &version_cmd)
    {
        if ver.name == "outbe-up" && ver.binary_sha256 == expected_sha256 {
            return Ok(handle);
        }
        if !allow_install {
            bail!(
                "pinned outbe-up on {} reported binary_sha256 `{}`, expected `{expected_sha256}`",
                ssh.host,
                ver.binary_sha256
            );
        }
    } else if !allow_install {
        bail!(
            "pinned outbe-up ({}) is missing on host {}; run `outbe-net-up network deploy` first",
            &expected_sha256[..16],
            ssh.host
        );
    }

    // Install pinned binary into `<state>/outbe-up/bin/outbe-up-<sha16>` without touching `$PATH`.
    let bytes = extract_bundled_outbe_up_bytes(artifacts, expected_sha256)?;
    let sha16 = &expected_sha256[..16];
    let install_cmd = format!(
        "set -eu; umask 077; dir={dir_expr}; state_dir=\"$(dirname \"$dir\")\"; mkdir -p \"$dir\"; chmod 0700 \"$state_dir\" \"$dir\"; tmp=\"$dir/.outbe-up-{sha16}.tmp.$$\"; cat > \"$tmp\"; chmod 0700 \"$tmp\"; mv -f \"$tmp\" {bin_expr}"
    );
    exec.capture_stdin(ssh, &ssh.host, &install_cmd, &bytes)
        .wrap_err_with(|| format!("install pinned outbe-up-{sha16} on {}", ssh.host))?;

    let ver: RemoteVersionOutput = run_host_json(exec, ssh, &ssh.host, &version_cmd)
        .wrap_err_with(|| {
            format!(
                "verify installed outbe-up-{sha16} via `version --json` on {}",
                ssh.host
            )
        })?;
    ensure!(
        ver.name == "outbe-up" && ver.binary_sha256 == expected_sha256,
        "installed outbe-up on {} failed SHA-256 verification: got name=`{}` sha256=`{}`, expected `outbe-up` / `{expected_sha256}` (version={})",
        ssh.host,
        ver.name,
        ver.binary_sha256,
        ver.version
    );
    Ok(handle)
}

// ---------------------------------------------------------------------------
// Host queries, image transfer, tar streaming, and host checks
// ---------------------------------------------------------------------------

struct UniqueHost {
    ssh: Ssh,
    vmm_home: String,
    bridge: String,
}

fn collect_unique_hosts(members: &[DesiredMember]) -> BTreeMap<String, UniqueHost> {
    let mut hosts = BTreeMap::new();
    for m in members {
        let key = host_key(&m.vmm_host, &m.vmm_home);
        hosts.entry(key).or_insert_with(|| UniqueHost {
            ssh: m.vmm_host.clone(),
            vmm_home: m.vmm_home.clone(),
            bridge: m.bridge.clone(),
        });
    }
    hosts
}

fn build_member_state(
    member: &DesiredMember,
    image_hash: &str,
    disk_source_sha256: Option<String>,
    vm_id: Option<String>,
    mac: Option<String>,
) -> NetworkMemberState {
    NetworkMemberState {
        index: member.index,
        role: member.role,
        name: member.name.clone(),
        vm_name: member.vm_name.clone(),
        owner_tag: member.owner_tag.clone(),
        vmm_host: member.vmm_host.clone(),
        vmm_home: member.vmm_home.clone(),
        bridge: member.bridge.clone(),
        service_ip: member.service_ip,
        advertise_ip: member.advertise_ip,
        port_offset: member.port_offset,
        vcpu: member.vcpu,
        memory_gib: member.memory_gib,
        disk_gib: member.disk_gib,
        flavor: member.flavor,
        no_tee: member.no_tee,
        image_hash: image_hash.to_owned(),
        disk_source_sha256,
        vm_id,
        mac,
    }
}

fn build_desired_members(
    build: &NetworkArtifacts,
    state: Option<&NetworkVmState>,
    no_tee_override: Option<bool>,
    image_name: Option<&str>,
    image_hash: Option<&str>,
) -> Result<Vec<DesiredMember>> {
    let mut out = Vec::with_capacity(build.config.founders.len());
    for (index, founder) in build.config.founders.iter().enumerate() {
        let resolved: ResolvedFounderVm = build.config.resolve_founder_vm(founder)?;
        let vm_name = founder_vm_name(&build.config.name, &founder.name);
        let owner_tag = member_owner_tag(
            &build.config.name,
            &build.manifest.genesis.final_sha256,
            &founder.name,
        );
        let prev = state.and_then(|st| st.member(&vm_name));
        let no_tee = no_tee_override
            .or_else(|| prev.map(|m| m.no_tee))
            .unwrap_or(false);
        let eff_image_name = image_name.map(ToOwned::to_owned).or_else(|| {
            state
                .filter(|st| !st.image_name.is_empty())
                .map(|st| st.image_name.clone())
        });
        let eff_image_hash = image_hash
            .map(ToOwned::to_owned)
            .or_else(|| {
                state
                    .filter(|st| !st.image_hash.is_empty())
                    .map(|st| st.image_hash.clone())
            })
            .or_else(|| {
                prev.filter(|m| !m.image_hash.is_empty())
                    .map(|m| m.image_hash.clone())
            });
        out.push(DesiredMember {
            index,
            role: MemberRole::Founder,
            name: founder.name.clone(),
            vm_name,
            owner_tag,
            vmm_host: resolved.vmm_host,
            vmm_home: resolved.vmm_home,
            bridge: resolved.bridge,
            service_ip: resolved.bridge_ip,
            advertise_ip: founder.advertise_ip,
            port_offset: resolved.port_offset,
            vcpu: resolved.vcpu,
            memory_gib: resolved.memory_gib,
            disk_gib: resolved.disk_gib,
            flavor: resolved.flavor,
            no_tee,
            image_name: eff_image_name,
            image_hash: eff_image_hash,
            recorded_disk_sha256: prev.and_then(|m| m.disk_source_sha256.clone()),
        });
    }
    Ok(out)
}

/// Build desired members for all roles: founders from `network.yaml` plus any added
/// `joining-validator` and `full-node` members recorded in `network-vm-state.json`.
fn build_all_desired_members(
    build: &NetworkArtifacts,
    state: Option<&NetworkVmState>,
    no_tee_override: Option<bool>,
    image_name: Option<&str>,
    image_hash: Option<&str>,
) -> Result<Vec<DesiredMember>> {
    let mut out = build_desired_members(build, state, no_tee_override, image_name, image_hash)?;
    if let Some(st) = state {
        for m in &st.members {
            if m.role == MemberRole::Founder
                || out.iter().any(|existing| existing.vm_name == m.vm_name)
            {
                continue;
            }
            let eff_image_name = if !st.image_name.is_empty() && st.image_hash == m.image_hash {
                Some(st.image_name.clone())
            } else {
                None
            };
            let eff_image_hash = (!m.image_hash.is_empty()).then(|| m.image_hash.clone());
            out.push(DesiredMember {
                index: m.index,
                role: m.role,
                name: m.name.clone(),
                vm_name: m.vm_name.clone(),
                owner_tag: m.owner_tag.clone(),
                vmm_host: m.vmm_host.clone(),
                vmm_home: m.vmm_home.clone(),
                bridge: m.bridge.clone(),
                service_ip: m.service_ip,
                advertise_ip: m.advertise_ip,
                port_offset: m.port_offset,
                vcpu: m.vcpu,
                memory_gib: m.memory_gib,
                disk_gib: m.disk_gib,
                flavor: m.flavor,
                no_tee: m.no_tee,
                image_name: eff_image_name,
                image_hash: eff_image_hash,
                recorded_disk_sha256: m.disk_source_sha256.clone(),
            });
        }
    }
    Ok(out)
}

fn fetch_host_vms(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    vmm_home: &str,
) -> Result<Vec<RemoteVmStatusOutput>> {
    let cmd = bin.cmd(&format!(
        "vm status --vmm-home {} --json",
        shell_quote_tilde_path(vmm_home)
    ));
    run_host_json(exec, ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("query `outbe-up vm status --json` on {}", ssh.host))
}

fn fetch_host_staged_disks(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
) -> Result<Vec<RemoteStagedDiskInfo>> {
    let cmd = bin.cmd("disk clean --list --json");
    run_host_json(exec, ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("list staged disks on {}", ssh.host))
}

fn can_reuse_staged_disk(
    entry: Option<&RemoteStagedDiskInfo>,
    expected_sha256: Option<&str>,
) -> bool {
    let (Some(entry), Some(expected)) = (entry, expected_sha256) else {
        return false;
    };
    entry.error.is_none() && entry.sha256.as_deref() == Some(expected)
}

/// Build an in-memory deterministic POSIX tar archive of a founder data tree for streaming over
/// stdin to `outbe-up disk prepare --role founder --source - --staging-name <vm>`.
pub(crate) fn build_founder_tar_bytes(founder_tree: &Path) -> Result<Vec<u8>> {
    ensure!(
        founder_tree.is_dir(),
        "founder data tree does not exist: {}",
        founder_tree.display()
    );
    let mut paths = Vec::new();
    for entry in WalkDir::new(founder_tree).follow_links(false) {
        let entry = entry?;
        if entry.path() == founder_tree {
            continue;
        }
        paths.push(entry.into_path());
    }
    paths.sort();

    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        builder.follow_symlinks(false);
        for path in paths {
            let rel = path.strip_prefix(founder_tree)?;
            let meta = fs::symlink_metadata(&path)?;
            let ft = meta.file_type();
            ensure!(
                !ft.is_symlink(),
                "refusing symlink in founder data tree: {}",
                path.display()
            );
            let mode = meta.permissions().mode() & 0o777;
            if ft.is_dir() {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(mode);
                header.set_size(0);
                header.set_mtime(0);
                header.set_uid(0);
                header.set_gid(0);
                header.set_cksum();
                builder.append_data(&mut header, rel, std::io::empty())?;
            } else if ft.is_file() {
                ensure!(
                    mode & 0o111 == 0,
                    "refusing executable file in founder data tree: {}",
                    path.display()
                );
                let data = fs::read(&path)?;
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(mode);
                header.set_size(data.len() as u64);
                header.set_mtime(0);
                header.set_uid(0);
                header.set_gid(0);
                header.set_cksum();
                builder.append_data(&mut header, rel, data.as_slice())?;
            } else {
                bail!(
                    "refusing non-regular entry in founder data tree: {}",
                    path.display()
                );
            }
        }
        builder.finish()?;
    }
    Ok(out)
}

enum ResolvedImageSource {
    Archive { path: PathBuf, sha256: String },
    Url { url: String, sha256: String },
}

fn resolve_image_source(source: &ImageSource) -> Result<(ResolvedImageSource, String)> {
    match source {
        ImageSource::Archive { path, sha256 } => {
            ensure!(
                path.is_file(),
                "guest OS image archive does not exist: {}",
                path.display()
            );
            let actual = sha256_file(path)?;
            if let Some(expected) = sha256 {
                let norm = normalize_sha256(expected)?;
                ensure!(
                    actual == norm,
                    "local image archive SHA-256 mismatch: expected {norm}, got {actual}"
                );
            }
            let id = format!("archive:{actual}");
            Ok((
                ResolvedImageSource::Archive {
                    path: path.clone(),
                    sha256: actual,
                },
                id,
            ))
        }
        ImageSource::Url { url, sha256 } => {
            ensure!(
                url.starts_with("https://") && !url.chars().any(char::is_whitespace),
                "image URL must be an https:// URL without whitespace (got `{}`)",
                url.split('?').next().unwrap_or(url)
            );
            let norm = normalize_sha256(sha256)?;
            let id = format!("url:{norm}");
            Ok((
                ResolvedImageSource::Url {
                    url: url.clone(),
                    sha256: norm,
                },
                id,
            ))
        }
    }
}

fn normalize_sha256(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    ensure!(
        trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected 64-character hex SHA-256 digest, got `{raw}`"
    );
    Ok(trimmed)
}

/// Read `os_image_hash` from `image-archive-manifest.json` inside a local `.tar.zst` guest image
/// archive so `import_image_on_host` can skip `scp` when the host already has the image.
fn read_local_archive_os_image_hash(local_archive: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct LocalArchiveManifest {
        os_image_hash: String,
    }

    let file = fs::File::open(local_archive).ok()?;
    let decoder = zstd::Decoder::new(file).ok()?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().ok()?;
    for entry in entries.flatten() {
        let path = entry.path().ok()?;
        let normalized = path
            .to_str()?
            .trim_start_matches("./")
            .trim_start_matches('/');
        if normalized == "image-archive-manifest.json" {
            let manifest: LocalArchiveManifest = serde_json::from_reader(entry).ok()?;
            return normalize_sha256(&manifest.os_image_hash).ok();
        }
    }
    None
}

fn import_image_on_host(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    vmm_home: &str,
    local_archive: &Path,
    sha256: &str,
) -> Result<RemoteImagePublishOutput> {
    // F7: check `outbe-up image list --json` first and skip uploading the archive via `scp` when
    // the host already has a verified managed image with the same `os_image_hash`.
    if let Some(expected_hash) = read_local_archive_os_image_hash(local_archive) {
        let list_cmd = bin.cmd(&format!(
            "image list --vmm-home {} --json",
            shell_quote_tilde_path(vmm_home)
        ));
        if let Ok(listed) =
            run_host_json::<dyn HostExec, Vec<RemoteListedImage>>(exec, ssh, &ssh.host, &list_cmd)
        {
            if let Some(existing) = listed
                .into_iter()
                .find(|img| img.has_archive_manifest && img.os_image_hash == expected_hash)
            {
                return Ok(RemoteImagePublishOutput {
                    image_name: existing.name,
                    os_image_hash: existing.os_image_hash,
                    target_dir: existing.path,
                    reused: true,
                });
            }
        }
    }

    let filename = local_archive
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            eyre::eyre!(
                "image archive path has no UTF-8 filename: {}",
                local_archive.display()
            )
        })?;
    let remote_dir = make_remote_temp_dir(exec, ssh, "outbe-net-up-img")?;
    let remote_archive = format!("{remote_dir}/{filename}");
    let res = (|| {
        exec.copy_to(ssh, &ssh.host, &[local_archive], &remote_dir, false)
            .wrap_err_with(|| format!("upload image archive to {}", ssh.host))?;
        let import_cmd = bin.cmd(&format!(
            "image import --archive {} --sha256 {} --vmm-home {} --json",
            shell_quote(&remote_archive),
            shell_quote(sha256),
            shell_quote_tilde_path(vmm_home),
        ));
        run_host_json(exec, ssh, &ssh.host, &import_cmd)
            .wrap_err_with(|| format!("import guest OS image on {}", ssh.host))
    })();
    let _ = cleanup_remote_dir(exec, ssh, &remote_dir);
    res
}

fn fetch_image_on_host(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    vmm_home: &str,
    url: &str,
    sha256: &str,
) -> Result<RemoteImagePublishOutput> {
    let fetch_cmd = bin.cmd(&format!(
        "image fetch --url {} --sha256 {} --vmm-home {} --json",
        shell_quote(url),
        shell_quote(sha256),
        shell_quote_tilde_path(vmm_home),
    ));
    run_host_json(exec, ssh, &ssh.host, &fetch_cmd)
        .wrap_err_with(|| format!("fetch guest OS image on {}", ssh.host))
}

fn make_remote_temp_dir(exec: &dyn HostExec, ssh: &Ssh, prefix: &str) -> Result<String> {
    let cmd = format!(
        "d=$(mktemp -d /tmp/{prefix}.XXXXXX) && chmod 0700 \"$d\" && printf '%s\\n' \"$d\""
    );
    let path = exec
        .capture(ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("create remote staging directory on {}", ssh.host))?;
    let expected_prefix = format!("/tmp/{prefix}.");
    ensure!(
        path.starts_with(&expected_prefix)
            && path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')),
        "remote host {} returned unsafe staging path `{path}`",
        ssh.host
    );
    Ok(path)
}

fn cleanup_remote_dir(exec: &dyn HostExec, ssh: &Ssh, remote_dir: &str) -> Result<()> {
    ensure!(
        remote_dir.starts_with("/tmp/outbe-net-up-"),
        "refusing to remove non-staging remote path `{remote_dir}`"
    );
    exec.run_quiet(
        ssh,
        &ssh.host,
        &format!("rm -rf -- {}", shell_quote(remote_dir)),
    )
}

struct PublishContext {
    multi_host: bool,
    peer_sources: Vec<String>,
}

impl PublishContext {
    fn from_members(members: &[DesiredMember]) -> Self {
        let distinct_hosts: BTreeSet<&str> =
            members.iter().map(|m| m.vmm_host.host.as_str()).collect();
        let peer_sources: Vec<String> = members
            .iter()
            .filter(|m| m.role != MemberRole::FullNode)
            .map(|m| format!("{}/32", m.advertise_ip))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            multi_host: distinct_hosts.len() > 1,
            peer_sources,
        }
    }

    fn requires_published_ports(&self, member: &DesiredMember) -> bool {
        if member.role == MemberRole::FullNode {
            return false;
        }
        self.multi_host || member.port_offset > 0 || member.advertise_ip != member.service_ip
    }

    fn p2p_port_specs(&self, member: &DesiredMember) -> Result<Vec<String>> {
        if !self.requires_published_ports(member) {
            return Ok(Vec::new());
        }
        let ports = FounderPublishedPorts::from_offset(member.port_offset)?;
        Ok(vec![
            format!("{}/tcp", ports.reth_p2p_tcp),
            format!("{}/udp", ports.reth_p2p_udp),
            format!("{}/tcp", ports.consensus_p2p_tcp),
            format!("{}/udp", ports.reth_discv5_udp),
            format!("{}/tcp", ports.radicle_tcp),
        ])
    }
}

fn build_host_check_command(
    member: &DesiredMember,
    bin: &RemoteOutbeUp,
    ctx: &PublishContext,
) -> Result<String> {
    let mut sub = format!(
        "host check --vm {} --ip {} --bridge {} --vmm-home {}",
        shell_quote(&member.vm_name),
        member.service_ip,
        shell_quote(&member.bridge),
        shell_quote_tilde_path(&member.vmm_home),
    );
    let ports = ctx.p2p_port_specs(member)?;
    if !ports.is_empty() {
        sub.push_str(&format!(" --public-ip {}", member.advertise_ip));
        for port in ports {
            sub.push_str(&format!(" --port {}", shell_quote(&port)));
        }
        for src in &ctx.peer_sources {
            sub.push_str(&format!(" --source {}", shell_quote(src)));
        }
    }
    sub.push_str(" --json");
    let inner = bin.cmd(&sub);
    Ok(format!(
        "rc=0; out=$({inner}) || rc=$?; if [ -z \"$out\" ] && [ \"$rc\" -ne 0 ]; then exit \"$rc\"; fi; printf '%s\\n{HOST_CHECK_EXIT_SENTINEL}%d\\n' \"$out\" \"$rc\""
    ))
}

fn run_host_checks(
    members: &[DesiredMember],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
    ctx: &PublishContext,
) -> Result<Vec<(String, RemoteHostCheckOutput)>> {
    let mut results = Vec::with_capacity(members.len());
    for member in members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &host_bins[&hkey];
        let cmd = build_host_check_command(member, bin, ctx)?;
        let raw = exec
            .capture_raw(&member.vmm_host, &member.name, &cmd)
            .wrap_err_with(|| {
                format!(
                    "run `outbe-up host check` for `{}` on {}",
                    member.vm_name, member.vmm_host.host
                )
            })?;
        let (mut report, exit_code) = parse_host_check_output(&raw, &member.vm_name)?;
        if exit_code != 0
            || !report.service_ip_ok
            || !report.host_network_ok
            || !report.published_ports_ok
        {
            report.ready = false;
        }
        results.push((member.vmm_host.host.clone(), report));
    }
    Ok(results)
}

fn parse_host_check_output(raw: &str, label: &str) -> Result<(RemoteHostCheckOutput, i32)> {
    let trimmed = raw.trim();
    if let Some(pos) = trimmed.rfind(HOST_CHECK_EXIT_SENTINEL) {
        let json_part = trimmed[..pos].trim();
        let rc_str = trimmed[pos + HOST_CHECK_EXIT_SENTINEL.len()..].trim();
        let rc: i32 = rc_str.parse().wrap_err_with(|| {
            format!(
                "invalid exit code sentinel from `outbe-up host check` on `{label}`: `{rc_str}`"
            )
        })?;
        let report: RemoteHostCheckOutput = serde_json::from_str(json_part)
            .wrap_err_with(|| format!("parse `outbe-up host check --json` output for `{label}`"))?;
        Ok((report, rc))
    } else {
        let report: RemoteHostCheckOutput = serde_json::from_str(trimmed)
            .wrap_err_with(|| format!("parse `outbe-up host check --json` output for `{label}`"))?;
        let rc = if report.ready { 0 } else { 1 };
        Ok((report, rc))
    }
}

fn format_failed_host_checks(checks: &[(String, RemoteHostCheckOutput)]) -> String {
    let mut out = String::new();
    for (host, check) in checks.iter().filter(|(_, c)| !c.ready) {
        out.push_str(&format!(
            "\nHost {host} / VM `{}` (host_network_ok={}, service_ip_ok={}, published_ports_ok={}):\n",
            check.vm, check.host_network_ok, check.service_ip_ok, check.published_ports_ok
        ));
        for conflict in &check.conflicts {
            out.push_str(&format!("  conflict: {conflict}\n"));
        }
        for missing in &check.missing {
            out.push_str(&format!("  missing:  {missing}\n"));
        }
        if !check.recommended_forward_rules.is_empty() {
            out.push_str("  Recommended forward rules:\n");
            for rule in &check.recommended_forward_rules {
                out.push_str(&format!("    {rule}\n"));
            }
        }
        if !check.recommended_prerouting_rules.is_empty() {
            out.push_str("  Recommended prerouting rules:\n");
            for rule in &check.recommended_prerouting_rules {
                out.push_str(&format!("    {rule}\n"));
            }
        }
        if !check.recommended_firewall_commands.is_empty() {
            out.push_str("  Apply firewall changes:\n");
            for cmd in &check.recommended_firewall_commands {
                out.push_str(&format!("    {cmd}\n"));
            }
        }
    }
    out.trim_end().to_owned()
}

// ---------------------------------------------------------------------------
// Batched JSON-RPC over SSH ControlMaster + Readiness + Status rendering
// ---------------------------------------------------------------------------

#[derive(Default)]
struct BatchedRpcResult {
    chain_id: Option<u64>,
    finalized: Option<u64>,
    finalized_hash: Option<String>,
    execution_peers: Option<u64>,
    consensus: Option<ConsensusStatus>,
}

/// Issue all 4 consensus/execution JSON-RPC queries in a single HTTP JSON-RPC batch request over
/// stdin to the VM's service IP (`http://<service_ip>:8545`, never DHCP IP).
fn query_vm_rpc_batch(
    exec: &dyn HostExec,
    ssh: &Ssh,
    label: &str,
    service_ip: Ipv4Addr,
) -> Result<BatchedRpcResult> {
    let url = format!("http://{service_ip}:8545");
    let batch_req = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []},
        {"jsonrpc": "2.0", "id": 2, "method": "eth_getBlockByNumber", "params": ["finalized", false]},
        {"jsonrpc": "2.0", "id": 3, "method": "net_peerCount", "params": []},
        {"jsonrpc": "2.0", "id": 4, "method": "outbe_consensusStatus", "params": []},
    ]);
    let payload = serde_json::to_vec(&batch_req)?;
    let cmd = format!(
        "curl -fsS --connect-timeout 5 --max-time 30 -H 'content-type: application/json' --data-binary @- {}",
        shell_quote(&url)
    );
    let raw = exec.capture_stdin(ssh, label, &cmd, &payload)?;
    let items: Vec<Value> = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse batched JSON-RPC response from {url}"))?;

    let mut out = BatchedRpcResult::default();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if item.get("error").is_some_and(|e| !e.is_null()) {
            continue;
        }
        let Some(result) = item.get("result") else {
            continue;
        };
        if result.is_null() {
            continue;
        }
        match id {
            1 => out.chain_id = parse_hex_u64(result),
            2 => {
                out.finalized = result.get("number").and_then(parse_hex_u64);
                out.finalized_hash = result
                    .get("hash")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            3 => out.execution_peers = parse_hex_u64(result),
            4 => {
                out.consensus = serde_json::from_value(result.clone()).ok();
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Fetch the canonical block hash at `height` from a VM's service IP over SSH ControlMaster.
fn query_vm_block_hash(
    exec: &dyn HostExec,
    ssh: &Ssh,
    label: &str,
    service_ip: Ipv4Addr,
    height: u64,
) -> Result<String> {
    let url = format!("http://{service_ip}:8545");
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": [format!("0x{height:x}"), false]
    });
    let payload = serde_json::to_vec(&req)?;
    let cmd = format!(
        "curl -fsS --connect-timeout 5 --max-time 30 -H 'content-type: application/json' --data-binary @- {}",
        shell_quote(&url)
    );
    let raw = exec.capture_stdin(ssh, label, &cmd, &payload)?;
    let resp: Value = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse block hash JSON-RPC response from {url}"))?;
    resp.pointer("/result/hash")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            eyre::eyre!("VM {label} ({service_ip}) returned no block hash for height {height}")
        })
}

fn parse_hex_u64(value: &Value) -> Option<u64> {
    let raw = value.as_str()?.trim();
    let hex = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))?;
    u64::from_str_radix(hex, 16).ok()
}

async fn query_members_and_orphans(
    build: &NetworkArtifacts,
    members: &[DesiredMember],
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &Arc<dyn HostExec>,
) -> Result<(Vec<VmMemberStatus>, Vec<OrphanVmStatus>)> {
    let mut host_vms: BTreeMap<String, Vec<RemoteVmStatusOutput>> = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let bin = host_bins[hkey].clone();
        let ssh = host.ssh.clone();
        let vmm_home = host.vmm_home.clone();
        let exec = Arc::clone(exec);
        let vms = tokio::task::spawn_blocking(move || {
            fetch_host_vms(exec.as_ref(), &ssh, &bin, &vmm_home)
        })
        .await
        .wrap_err("host vm status task panicked")??;
        host_vms.insert(hkey.clone(), vms);
    }

    let tag_prefix =
        network_owner_tag_prefix(&build.config.name, &build.manifest.genesis.final_sha256);
    let expected_names: BTreeSet<&str> = members.iter().map(|m| m.vm_name.as_str()).collect();
    let mut orphans = Vec::new();
    for (hkey, vms) in &host_vms {
        let host = &unique_hosts[hkey];
        for vm in vms {
            if vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&tag_prefix))
                && !expected_names.contains(vm.name.as_str())
            {
                orphans.push(OrphanVmStatus {
                    host: host.ssh.host.clone(),
                    vm_name: vm.name.clone(),
                    vm_id: vm.vm_id.clone(),
                    owner_tag: vm.owner_tag.clone().unwrap_or_default(),
                    vmm_status: vm.vmm_status.clone(),
                });
            }
        }
    }

    let mut tasks = JoinSet::new();
    for member in members.iter().cloned() {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let observed = host_vms[&hkey]
            .iter()
            .find(|vm| vm.name == member.vm_name)
            .cloned();
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || {
            build_member_status(member, observed.as_ref(), exec.as_ref(), true)
        });
    }

    let mut statuses = Vec::with_capacity(members.len());
    while let Some(res) = tasks.join_next().await {
        statuses.push(res.wrap_err("member status task panicked")?);
    }
    statuses.sort_by_key(|s| {
        members
            .iter()
            .position(|m| m.vm_name == s.vm_name)
            .unwrap_or(usize::MAX)
    });
    Ok((statuses, orphans))
}

fn build_member_status(
    member: DesiredMember,
    observed: Option<&RemoteVmStatusOutput>,
    exec: &dyn HostExec,
    query_rpc: bool,
) -> VmMemberStatus {
    let class = classify_host_vm(&member, observed);
    let kind = class.kind();
    let detail = match &class {
        VmClassification::Foreign { reason, .. } => Some(reason.clone()),
        VmClassification::Stale { reasons, .. } => Some(reasons.join("; ")),
        VmClassification::Partial { observed } => Some(format!("phase={}", observed.phase)),
        VmClassification::Absent | VmClassification::Ours { .. } => None,
    };

    let Some(vm) = observed else {
        return VmMemberStatus {
            name: member.name,
            role: member.role,
            vm_name: member.vm_name,
            vm_id: String::new(),
            host: member.vmm_host.host,
            classification: kind,
            classification_detail: detail,
            service_ip: member.service_ip,
            dhcp_ip: None,
            mac: String::new(),
            vmm_status: "absent".to_owned(),
            vmm_running: false,
            no_tee: member.no_tee,
            chain_id: None,
            finalized: None,
            finalized_hash: None,
            execution_peers: None,
            consensus_mode: None,
            consensus_peers: None,
            has_threshold_shares: None,
            vrf_material_version: None,
            tee_ready: None,
            consensus: None,
            boot_warning: None,
            error: Some("VM not created on host".to_owned()),
        };
    };

    // Keep VMM `boot_error` as a non-fatal warning when live checks pass (e.g. dstack-vmm
    // permanently latches a transient `dstack-guest-agent.service` restart even after recovery).
    // Surface it inside `error` whenever live checks fail (`!running` or RPC unreachable).
    let boot_warning = vm.boot_error.clone().filter(|s| !s.trim().is_empty());
    let mut error = vm.error.clone().filter(|s| !s.trim().is_empty());
    let effective_no_tee = vm.no_tee;
    let service_ip = vm
        .service_ip
        .or(vm.intended_ip)
        .unwrap_or(member.service_ip);

    if !vm.running || !query_rpc || kind != VmClassKind::Ours {
        if !vm.running && error.is_none() {
            error.clone_from(&boot_warning);
        }
        return VmMemberStatus {
            name: member.name,
            role: member.role,
            vm_name: member.vm_name,
            vm_id: vm.vm_id.clone(),
            host: member.vmm_host.host,
            classification: kind,
            classification_detail: detail,
            service_ip,
            dhcp_ip: vm.dhcp_ip,
            mac: vm.mac.clone(),
            vmm_status: vm.vmm_status.clone(),
            vmm_running: vm.running,
            no_tee: effective_no_tee,
            chain_id: None,
            finalized: None,
            finalized_hash: None,
            execution_peers: None,
            consensus_mode: None,
            consensus_peers: None,
            has_threshold_shares: None,
            vrf_material_version: None,
            tee_ready: None,
            consensus: None,
            boot_warning,
            error,
        };
    }

    let rpc = match query_vm_rpc_batch(exec, &member.vmm_host, &member.name, service_ip) {
        Ok(res) => res,
        Err(err) => {
            if error.is_none() {
                error = Some(format_rpc_error_with_boot_warning(
                    format!("RPC unreachable on service IP {service_ip}: {err:#}"),
                    boot_warning.as_deref(),
                ));
            }
            BatchedRpcResult::default()
        }
    };
    if (rpc.chain_id.is_none() || rpc.finalized.is_none()) && error.is_none() {
        error = Some(format_rpc_error_with_boot_warning(
            format!("RPC unreachable on service IP {service_ip}"),
            boot_warning.as_deref(),
        ));
    }

    let tee_ready = if effective_no_tee {
        None
    } else {
        rpc.consensus.as_ref().map(ConsensusStatus::resident_ready)
    };

    VmMemberStatus {
        name: member.name,
        role: member.role,
        vm_name: member.vm_name,
        vm_id: vm.vm_id.clone(),
        host: member.vmm_host.host,
        classification: kind,
        classification_detail: detail,
        service_ip,
        dhcp_ip: vm.dhcp_ip,
        mac: vm.mac.clone(),
        vmm_status: vm.vmm_status.clone(),
        vmm_running: vm.running,
        no_tee: effective_no_tee,
        chain_id: rpc.chain_id,
        finalized: rpc.finalized,
        finalized_hash: rpc.finalized_hash,
        execution_peers: rpc.execution_peers,
        consensus_mode: rpc.consensus.as_ref().map(|c| c.mode.clone()),
        consensus_peers: rpc.consensus.as_ref().map(|c| c.connected_peers),
        has_threshold_shares: rpc.consensus.as_ref().map(|c| c.has_threshold_shares),
        vrf_material_version: rpc.consensus.as_ref().map(|c| c.vrf_material_version),
        tee_ready,
        consensus: rpc.consensus,
        boot_warning,
        error,
    }
}

fn format_rpc_error_with_boot_warning(base: String, boot_warning: Option<&str>) -> String {
    match boot_warning {
        Some(warn) => format!("{base} (boot_error: {warn})"),
        None => base,
    }
}

pub(crate) fn member_node_ready(
    status: &VmMemberStatus,
    chain_id: u64,
    required_peers: usize,
) -> bool {
    if !status.vmm_running || status.error.is_some() {
        return false;
    }
    if status.chain_id != Some(chain_id) || status.finalized.is_none() {
        return false;
    }
    if status.role == MemberRole::FullNode {
        // Keyless full nodes sync from upstream/bootnodes without validator signing keys or TEE.
        return true;
    }
    if status.execution_peers.unwrap_or(0) < required_peers as u64 {
        return false;
    }
    let Some(consensus) = status.consensus.as_ref() else {
        return false;
    };
    let core_ready = consensus.is_validator
        && consensus.mode == "signer"
        && consensus.has_threshold_shares
        && consensus.connected_peers >= required_peers as u64;
    if !core_ready {
        return false;
    }
    // Amendment 5: skip TEE enclave check when `no_tee` is enabled.
    if status.no_tee {
        true
    } else {
        consensus.resident_ready()
    }
}

/// Compute readiness baseline `B = max(finalized)` from the first sample where every member
/// reports a finalized block height. Using `max` ensures every node must advance past the
/// highest starting height (`>= B + min_finalized_blocks`), proving actual chain growth even when
/// a lagging node catches up to a stalled chain.
pub(crate) fn compute_readiness_baseline(statuses: &[VmMemberStatus]) -> Option<u64> {
    let heights: Option<Vec<u64>> = statuses.iter().map(|s| s.finalized).collect();
    heights?.into_iter().max()
}

pub(crate) fn evaluate_network_readiness(
    statuses: &[VmMemberStatus],
    assessments: &[MemberLagAssessment],
    chain_id: u64,
    required_peers: usize,
    baseline_b: Option<u64>,
    min_finalized_blocks: u64,
) -> bool {
    if statuses.is_empty() {
        return false;
    }
    let Some(b) = baseline_b else {
        return false;
    };
    let target_height = b.saturating_add(min_finalized_blocks);

    // Every member must be individually ready, non-lagging, and >= B + N.
    let all_nodes_ok = statuses.iter().zip(assessments).all(|(s, a)| {
        s.classification == VmClassKind::Ours
            && member_node_ready(s, chain_id, required_peers)
            && !a.lagging
            && s.finalized.is_some_and(|h| h >= target_height)
    });
    if !all_nodes_ok {
        return false;
    }

    // All validator members must agree on a common `vrf_material_version`.
    let versions: BTreeSet<u64> = statuses
        .iter()
        .filter(|s| s.role != MemberRole::FullNode)
        .filter_map(|s| s.vrf_material_version)
        .collect();
    versions.len() == 1
}

pub(crate) fn assess_members_lag(
    statuses: &[VmMemberStatus],
    max_lag: u64,
) -> Vec<MemberLagAssessment> {
    let highest_finalized = statuses.iter().filter_map(|s| s.finalized).max();
    let mut vrf_counts: BTreeMap<u64, usize> = BTreeMap::new();
    for s in statuses {
        if s.role != MemberRole::FullNode {
            if let Some(v) = s.vrf_material_version {
                *vrf_counts.entry(v).or_insert(0) += 1;
            }
        }
    }
    let majority_vrf = vrf_counts
        .into_iter()
        .max_by_key(|(v, count)| (*count, *v))
        .map(|(v, _)| v);

    statuses
        .iter()
        .map(|s| {
            let mut reasons = Vec::new();
            let lag = match (highest_finalized, s.finalized) {
                (Some(high), Some(mine)) => {
                    let delta = high.saturating_sub(mine);
                    if delta > max_lag {
                        reasons.push(format!("lagging by {delta} blocks (max {max_lag})"));
                    }
                    Some(delta)
                }
                _ => None,
            };
            if s.role != MemberRole::FullNode {
                if let (Some(maj), Some(mine)) = (majority_vrf, s.vrf_material_version) {
                    if mine != maj {
                        reasons.push(format!("vrf_material_version={mine} (majority={maj})"));
                    }
                }
            }
            MemberLagAssessment {
                lag,
                lagging: !reasons.is_empty(),
                reasons,
            }
        })
        .collect()
}

fn render_vm_status(
    build: &NetworkArtifacts,
    statuses: &[VmMemberStatus],
    orphans: &[OrphanVmStatus],
    max_lag: u64,
) -> String {
    let assessments = assess_members_lag(statuses, max_lag);
    let mut lines = Vec::new();
    lines.push(format!(
        "VM Network: {} (chain {})",
        build.config.name, build.config.chain_id
    ));
    lines.push(format!(
        "{:<14} {:<10} {:<16} {:<10} {:<10} {:<8} {:<6} {:<6} {:<12} DETAIL",
        "MEMBER", "CLASS", "SERVICE_IP", "VMM", "MODE", "FINAL", "LAG", "VRF", "TEE"
    ));
    for (s, a) in statuses.iter().zip(&assessments) {
        let class_str = format!("{:?}", s.classification).to_ascii_lowercase();
        let mode = if s.role == MemberRole::FullNode {
            s.consensus_mode.as_deref().unwrap_or("full-node")
        } else {
            s.consensus_mode.as_deref().unwrap_or("-")
        };
        let finalized = s
            .finalized
            .map(|h| h.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let lag = a
            .lag
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let vrf = if s.role == MemberRole::FullNode {
            "-".to_owned()
        } else {
            s.vrf_material_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_owned())
        };
        let tee = if s.role == MemberRole::FullNode {
            "n/a(keyless)".to_owned()
        } else if s.no_tee {
            "skip(no-tee)".to_owned()
        } else {
            s.tee_ready
                .map(|r| if r { "ready" } else { "not-ready" }.to_owned())
                .unwrap_or_else(|| "-".to_owned())
        };
        let base_detail = s
            .error
            .as_deref()
            .or(s.classification_detail.as_deref())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                if a.reasons.is_empty() {
                    "ok".to_owned()
                } else {
                    a.reasons.join(", ")
                }
            });
        let detail = match s.boot_warning.as_deref() {
            Some(warn) if !base_detail.contains(warn) => format!("{base_detail} (warn: {warn})"),
            _ => base_detail,
        };
        lines.push(format!(
            "{:<14} {:<10} {:<16} {:<10} {:<10} {:<8} {:<6} {:<6} {:<12} {detail}",
            s.name, class_str, s.service_ip, s.vmm_status, mode, finalized, lag, vrf, tee
        ));
    }
    if !orphans.is_empty() {
        lines.push("\nOrphan VMs with this network's owner_tag prefix:".to_owned());
        for o in orphans {
            lines.push(format!(
                "  - {} ({}) on {} [status={}, tag={}]",
                o.vm_name, o.vm_id, o.host, o.vmm_status, o.owner_tag
            ));
        }
    }
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
fn build_timeout_report(
    build: &NetworkArtifacts,
    targets: &[(DesiredMember, RemoteVmStatusOutput)],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
    timeout: Duration,
    statuses: &[VmMemberStatus],
    max_lag: u64,
    baseline_b: Option<u64>,
    min_finalized_blocks: u64,
) -> String {
    let mut out = format!(
        "timed out after {}s waiting for finalized consensus growth (baseline={:?}, required=+{min_finalized_blocks}):\n\n{}",
        timeout.as_secs(),
        baseline_b,
        render_vm_status(build, statuses, &[], max_lag)
    );
    for (member, vm) in targets {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &host_bins[&hkey];
        let cmd = bin.cmd(&format!(
            "vm logs --name {} --expect-vm-id {} --lines {} --vmm-home {} --json",
            shell_quote(&member.vm_name),
            shell_quote(&vm.vm_id),
            SERIAL_LOG_TAIL_LINES,
            shell_quote_tilde_path(&member.vmm_home),
        ));
        if let Ok(logs) = run_host_json::<dyn HostExec, RemoteVmLogsOutput>(
            exec,
            &member.vmm_host,
            &member.name,
            &cmd,
        ) {
            if !logs.log.trim().is_empty() {
                out.push_str(&format!(
                    "\n\n--- serial log tail for {} ({}) ---\n{}",
                    member.vm_name,
                    member.vmm_host.host,
                    logs.log.trim_end()
                ));
            }
        }
    }
    out
}

fn host_key(ssh: &Ssh, vmm_home: &str) -> String {
    format!("{}@{}:{}|{}", ssh.user, ssh.host, ssh.port, vmm_home)
}

fn ensure_vm_deployment(build: &NetworkArtifacts) -> Result<()> {
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network artifacts at {} are not a VM deployment (deployment = {:?})",
        build.root.display(),
        build.manifest.deployment
    );
    Ok(())
}

/// Safely quote a path for remote POSIX shells while expanding a leading `~` via `$HOME`.
fn shell_quote_tilde_path(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".to_owned()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME/\"{}", shell_quote(rest))
    } else {
        shell_quote(path)
    }
}

#[path = "vm_members.rs"]
mod members;

pub use members::{add_full_node, add_validator, AddFullNodeOptions, AddValidatorOptions};
#[cfg(test)]
pub(crate) use members::{
    add_full_node_with_exec, add_validator_with_exec, ensure_local_member_keys, JoinChainDriver,
};

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
