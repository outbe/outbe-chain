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
//!   on drop); guest JSON-RPC is batched over stdin to each VM's service IP (`OUTBE_VM_IP:<rpc_port>`).

mod deploy;
mod host_check;
mod images;
mod members;
mod readiness;
mod remote_bin;
mod remove;
mod rpc;
mod start;
mod status;
mod stop;
mod types;
mod update;

#[cfg(test)]
#[path = "../vm_tests.rs"]
mod tests;

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use eyre::{ensure, Result};
use outbe_operator_remote::{shell_quote, HostExec, Ssh, SshControlSession};

use super::{build::DeploymentKind, remote::NetworkArtifacts};

pub use members::{add_full_node, add_validator, AddFullNodeOptions, AddValidatorOptions};
pub use update::{update, UpdateOptions};

#[cfg(test)]
pub(crate) use members::{
    add_full_node_with_exec, add_validator_with_exec, ensure_local_member_keys, JoinChainDriver,
};
#[cfg(test)]
pub(crate) use update::{
    all_validators_randomness_healthy, check_validators_connected_peers,
    compute_effective_dkg_guard_blocks, update_with_exec,
};

pub(crate) use deploy::deploy_with_exec;
pub(crate) use host_check::{format_failed_host_checks, run_member_host_checks, PublishContext};
pub(crate) use images::{
    publish_image_on_host, resolve_image_source, HostImageTarget, ResolvedImageSource,
};
pub(crate) use readiness::{
    assess_members_lag, compute_readiness_baseline, member_node_ready, render_vm_status,
};
pub(crate) use remote_bin::{
    build_founder_tar_bytes, cleanup_remote_dir, ensure_pinned_outbe_up_on_host,
    ensure_pinned_outbe_up_on_host_from_bundle, fetch_host_staged_disks, fetch_host_vms,
    make_remote_temp_dir, resolve_bundled_outbe_up_sha256_from_bundle,
    resolve_state_or_bundled_outbe_up_sha256, RemoteOutbeUp,
};
pub(crate) use remove::remove_with_exec;
pub(crate) use rpc::{
    ensure_vm_rpc_reachable_for_orchestration, load_and_verify_vm_orchestration,
    query_hyperlane_health, query_validators_missed_votes, query_vm_block_hash, VmRpcTarget,
};
pub(crate) use start::start_with_exec;
pub(crate) use status::{
    query_members_and_orphans, status_with_exec, MemberLagAssessment, OrphanVmStatus,
    VmMemberStatus,
};
pub(crate) use stop::stop_with_exec;
pub(crate) use types::{
    build_all_desired_members, build_member_state, can_reuse_staged_disk, classify_host_vm,
    collect_unique_hosts, founder_vm_name, member_owner_tag, DesiredMember,
    RemoteCleanStagingOutput, RemoteCleanStaleOutput, RemoteDiskPrepareOutput,
    RemoteHostStatusOutput, RemoteImagePublishOutput, RemoteListedImage, RemoteStagedDiskInfo,
    RemoteVmCreateOutput, RemoteVmLogsOutput, RemoteVmStartOutput, RemoteVmStatusOutput,
    RemoteVmUpdateOutput, UniqueHost, VmClassKind, VmClassification,
};

#[cfg(test)]
use super::config::VmFlavor;
#[cfg(test)]
use host_check::HOST_CHECK_EXIT_SENTINEL;
#[cfg(test)]
use outbe_operator_node_role::rpc::ConsensusStatus;
#[cfg(test)]
use readiness::{evaluate_network_readiness, ReadinessRequirements};
#[cfg(test)]
use rpc::{is_private_rpc_url, query_certified_rpc_chain_id_local, query_vm_rpc_batch};
#[cfg(test)]
use status::{build_member_status, check_unhealthy_members};
#[cfg(test)]
use std::time::Instant;

pub const EXIT_CODE_HOST_CONFIG_REQUIRED: u8 = 2;
pub(super) const SERIAL_LOG_TAIL_LINES: usize = 40;
pub(super) const READY_CONSECUTIVE_SAMPLES: u32 = 2;

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

async fn with_ssh_session<F, Fut>(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    action: F,
) -> Result<()>
where
    F: FnOnce(Arc<dyn HostExec>) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let build = NetworkArtifacts::load(artifacts)?;
    let session = Arc::new(SshControlSession::new_for_network(
        ssh_key,
        ssh_config,
        &build.manifest.genesis.final_sha256,
    )?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = action(exec).await;
    session.close_all();
    res
}

pub async fn deploy(options: DeployOptions<'_>) -> Result<()> {
    with_ssh_session(
        options.artifacts,
        options.ssh_key,
        options.ssh_config,
        |exec| async move { deploy_with_exec(&options, exec).await },
    )
    .await
}

pub async fn start(options: StartOptions<'_>) -> Result<()> {
    with_ssh_session(
        options.artifacts,
        options.ssh_key,
        options.ssh_config,
        |exec| async move { start_with_exec(&options, exec).await },
    )
    .await
}

pub async fn stop(options: StopOptions<'_>) -> Result<()> {
    with_ssh_session(
        options.artifacts,
        options.ssh_key,
        options.ssh_config,
        |exec| async move { stop_with_exec(&options, exec).await },
    )
    .await
}

pub async fn status(options: StatusOptions<'_>) -> Result<()> {
    with_ssh_session(
        options.artifacts,
        options.ssh_key,
        options.ssh_config,
        |exec| async move { status_with_exec(&options, exec).await },
    )
    .await
}

pub async fn remove(options: RemoveOptions<'_>) -> Result<()> {
    with_ssh_session(
        options.artifacts,
        options.ssh_key,
        options.ssh_config,
        |exec| async move { remove_with_exec(&options, exec).await },
    )
    .await
}

pub(super) fn host_key(ssh: &Ssh, vmm_home: &str) -> String {
    format!("{}@{}:{}|{}", ssh.user, ssh.host, ssh.port, vmm_home)
}

pub(super) fn ensure_vm_deployment(build: &NetworkArtifacts) -> Result<()> {
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network artifacts at {} are not a VM deployment (deployment = {:?})",
        build.root.display(),
        build.manifest.deployment
    );
    Ok(())
}

/// Safely quote a path for remote POSIX shells while expanding a leading `~` via `$HOME`.
pub(super) fn shell_quote_tilde_path(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".to_owned()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME/\"{}", shell_quote(rest))
    } else {
        shell_quote(path)
    }
}
