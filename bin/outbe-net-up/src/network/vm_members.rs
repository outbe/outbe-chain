//! Joining-validator and keyless full-node VM orchestration (`outbe-net-up network add-validator|add-full-node`).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::{Address, U256};
use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    config::{
        FullNodeConfig, FullNodeNetworkConfig, NodeRole, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
    progress::Progress,
    rpc::{Block, RpcClient, ValidatorRecord},
    validator_address,
    vm_network::{
        StaticVmNetworkConfig, DEFAULT_CONSENSUS_PORT, DEFAULT_DISCV5_PORT, DEFAULT_RADICLE_PORT,
        DEFAULT_RETH_PORT,
    },
    FEEDER_FUNDING_WEI,
};
use outbe_operator_remote::{
    run_host_json, shell_quote, validate_ssh, HostExec, Ssh, SshControlSession,
};
use outbe_software_bundle::{sha256_file, verify_and_extract};
use walkdir::WalkDir;

use super::{
    build_all_desired_members, build_founder_tar_bytes, build_host_check_command,
    build_member_state, can_reuse_staged_disk, classify_host_vm, cleanup_remote_dir,
    collect_unique_hosts, ensure_pinned_outbe_up_on_host, ensure_vm_deployment,
    fetch_host_staged_disks, fetch_host_vms, fetch_image_on_host, format_failed_host_checks,
    founder_vm_name, host_key, import_image_on_host, make_remote_temp_dir, member_node_ready,
    member_owner_tag, parse_host_check_output, query_members_and_orphans, query_vm_block_hash,
    render_vm_status, resolve_image_source, resolve_state_or_bundled_outbe_up_sha256,
    shell_quote_tilde_path, DesiredMember, ExitCodeError, ImageSource, PublishContext,
    RemoteCleanStagingOutput, RemoteCleanStaleOutput, RemoteDiskPrepareOutput,
    RemoteHostStatusOutput, RemoteImagePublishOutput, RemoteListedImage, RemoteOutbeUp,
    RemoteVmCreateOutput, RemoteVmLogsOutput, RemoteVmStartOutput, ResolvedImageSource,
    VmClassKind, VmClassification, VmMemberStatus, EXIT_CODE_HOST_CONFIG_REQUIRED,
    READY_CONSECUTIVE_SAMPLES, SERIAL_LOG_TAIL_LINES,
};
use crate::network::{
    config::{FounderPublishedPorts, VmFlavor},
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState},
};

const ORACLE_DELEGATE_ROLE: u8 = 1;
/// Explicit gas reserve (2 COEN) funded onto a joining validator alongside `min_stake` and
/// `FEEDER_FUNDING_WEI` BEFORE `vm start`, covering paid `registerEnclave` (not ZeroFee),
/// `staking stake`, `validator delegate oracle`, feeder funding, and `validator confirm-ready`.
const JOINER_GAS_RESERVE_WEI: u128 = 2_000_000_000_000_000_000;
const REQUIRED_MEMBER_KEYS: &[&str] = &[
    "signing-key.hex",
    "evm-key.hex",
    "feeder-key.hex",
    "hyperlane-key.hex",
    "reth-p2p-secret.hex",
    "radicle/keys/radicle",
];

#[derive(Clone, Debug)]
pub struct AddValidatorOptions<'a> {
    pub artifacts: &'a Path,
    pub name: &'a str,
    pub host: &'a str,
    pub user: Option<&'a str>,
    pub port: u16,
    pub vmm_home: Option<&'a str>,
    pub bridge: Option<&'a str>,
    pub bridge_ip: Ipv4Addr,
    pub advertise_ip: Ipv4Addr,
    pub port_offset: u16,
    pub image_source: Option<ImageSource>,
    pub vcpu: Option<u32>,
    pub memory_gib: Option<u32>,
    pub disk_gib: Option<u32>,
    pub no_tee: bool,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
}

#[derive(Clone, Debug)]
pub struct AddFullNodeOptions<'a> {
    pub artifacts: &'a Path,
    pub name: &'a str,
    pub host: &'a str,
    pub user: Option<&'a str>,
    pub port: u16,
    pub vmm_home: Option<&'a str>,
    pub bridge: Option<&'a str>,
    pub bridge_ip: Ipv4Addr,
    pub advertise_ip: Option<Ipv4Addr>,
    pub image_source: Option<ImageSource>,
    pub vcpu: Option<u32>,
    pub memory_gib: Option<u32>,
    pub disk_gib: Option<u32>,
    pub no_tee: bool,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub timeout: Duration,
    pub max_lag: u64,
    pub min_finalized_blocks: u64,
}

/// Pluggable on-chain & `outbe-cli` driver for the joining-validator lifecycle.
///
/// Bare-host `crates/operator/node-role/src/promote.rs` cannot be reused directly here because it
/// executes on the validator host against `/opt/outbe-chain/keys` via `runuser --user outbe`; in
/// VM mode joiner keys stay strictly on the operator host (never placed on the VMM host) and
/// transactions are signed locally against `certified_rpc`. Tests inject a fake driver.
pub(crate) trait JoinChainDriver: Send + Sync {
    fn min_stake(&self) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>>;
    fn balance(&self, addr: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>>;
    fn validator(
        &self,
        addr: Address,
    ) -> Pin<Box<dyn Future<Output = Result<ValidatorRecord>> + Send + '_>>;
    fn validator_delegate(
        &self,
        validator: Address,
        role: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Address>> + Send + '_>>;
    fn finalized_block(&self) -> Pin<Box<dyn Future<Output = Result<Block>> + Send + '_>>;
    fn fund_address(
        &self,
        from_key: &Path,
        to: Address,
        amount: U256,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>>;
    fn stake(
        &self,
        joiner_keys_dir: &Path,
        validator: Address,
        amount: U256,
        symmetric_endpoint: &str,
    ) -> Result<String>;
    fn delegate_oracle(&self, joiner_evm_key: &Path, feeder: Address) -> Result<String>;
    fn confirm_ready(&self, joiner_evm_key: &Path) -> Result<String>;
    fn wait_tx_finalized(
        &self,
        hash: &str,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

struct RealJoinChainDriver {
    rpc_url: String,
    rpc: RpcClient,
    cli_bin: PathBuf,
    _extract_dir: tempfile::TempDir,
}

impl RealJoinChainDriver {
    fn from_artifacts(artifacts: &Path, rpc_url: &str) -> Result<Self> {
        let bundle = artifacts.join("software-bundle.tar.zst");
        let tmp = tempfile::Builder::new()
            .prefix("outbe-cli-join-")
            .tempdir()
            .wrap_err("create temp dir for bundled outbe-cli")?;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
        verify_and_extract(&bundle, tmp.path())
            .wrap_err("verify software bundle for outbe-cli extraction")?;
        let cli_bin = tmp.path().join("bin/outbe-cli");
        ensure!(
            cli_bin.is_file(),
            "software bundle {} is missing bin/outbe-cli",
            bundle.display()
        );
        let rpc = RpcClient::new(rpc_url)?;
        Ok(Self {
            rpc_url: rpc_url.to_owned(),
            rpc,
            cli_bin,
            _extract_dir: tmp,
        })
    }

    fn run_cli(&self, key_file: &Path, args: &[&str]) -> Result<String> {
        let key_str = key_file
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 key path: {}", key_file.display()))?;
        let output = Command::new(&self.cli_bin)
            .args(["--rpc-url", &self.rpc_url, "--private-key-file", key_str])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .wrap_err_with(|| format!("execute bundled {}", self.cli_bin.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        ensure!(
            output.status.success(),
            "outbe-cli {} failed ({}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
        extract_tx_hash(&stdout).or_else(|_| extract_tx_hash(&stderr))
    }
}

impl JoinChainDriver for RealJoinChainDriver {
    fn min_stake(&self) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>> {
        Box::pin(async move { self.rpc.min_stake().await })
    }

    fn balance(&self, addr: Address) -> Pin<Box<dyn Future<Output = Result<U256>> + Send + '_>> {
        Box::pin(async move { self.rpc.balance(addr).await })
    }

    fn validator(
        &self,
        addr: Address,
    ) -> Pin<Box<dyn Future<Output = Result<ValidatorRecord>> + Send + '_>> {
        Box::pin(async move { self.rpc.validator(addr).await })
    }

    fn validator_delegate(
        &self,
        validator: Address,
        role: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Address>> + Send + '_>> {
        Box::pin(async move { self.rpc.validator_delegate(validator, role).await })
    }

    fn finalized_block(&self) -> Pin<Box<dyn Future<Output = Result<Block>> + Send + '_>> {
        Box::pin(async move { self.rpc.finalized_block().await })
    }

    fn fund_address(
        &self,
        from_key: &Path,
        to: Address,
        amount: U256,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        let from_key = from_key.to_path_buf();
        Box::pin(async move {
            outbe_operator_node_role::hyperlane::send_value_transfer(
                &self.rpc, &from_key, to, amount,
            )
            .await
        })
    }

    fn stake(
        &self,
        joiner_keys_dir: &Path,
        validator: Address,
        amount: U256,
        symmetric_endpoint: &str,
    ) -> Result<String> {
        let evm_key = joiner_keys_dir.join("evm-key.hex");
        let keys_str = joiner_keys_dir
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 keys dir: {}", joiner_keys_dir.display()))?;
        let val_arg = format!("{validator:#x}");
        let amt_arg = amount.to_string();
        self.run_cli(
            &evm_key,
            &[
                "staking",
                "stake",
                "--validator",
                &val_arg,
                "--amount",
                &amt_arg,
                "--keys-dir",
                keys_str,
                "--symmetric",
                symmetric_endpoint,
            ],
        )
    }

    fn delegate_oracle(&self, joiner_evm_key: &Path, feeder: Address) -> Result<String> {
        let feeder_arg = format!("{feeder:#x}");
        self.run_cli(
            joiner_evm_key,
            &["validator", "delegate", "oracle", &feeder_arg],
        )
    }

    fn confirm_ready(&self, joiner_evm_key: &Path) -> Result<String> {
        self.run_cli(joiner_evm_key, &["validator", "confirm-ready"])
    }

    fn wait_tx_finalized(
        &self,
        hash: &str,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let hash = hash.to_owned();
        Box::pin(async move {
            loop {
                if let Some(block) = self.rpc.transaction_block(&hash).await? {
                    let finalized = self.rpc.finalized_block().await?.number;
                    if finalized >= block {
                        return Ok(());
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for transaction {hash} to finalize"
                );
                let sleep_for = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(3));
                tokio::time::sleep(sleep_for).await;
            }
        })
    }
}

pub async fn add_validator(options: AddValidatorOptions<'_>) -> Result<()> {
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;
    let driver = RealJoinChainDriver::from_artifacts(&build.root, &build.config.certified_rpc)?;
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = add_validator_with_exec(&options, exec, &driver).await;
    session.close_all();
    res
}

pub async fn add_full_node(options: AddFullNodeOptions<'_>) -> Result<()> {
    let session = Arc::new(SshControlSession::new(options.ssh_key, options.ssh_config)?);
    let exec: Arc<dyn HostExec> = session.clone();
    let res = add_full_node_with_exec(&options, exec).await;
    session.close_all();
    res
}

// ---------------------------------------------------------------------------
// Shared member VM provisioning (reconcile, disk prepare, vm create, host check)
// ---------------------------------------------------------------------------

struct ProvisionRequest<'a> {
    artifacts: &'a Path,
    role: MemberRole,
    name: &'a str,
    host: &'a str,
    user: Option<&'a str>,
    port: u16,
    vmm_home: Option<&'a str>,
    bridge: Option<&'a str>,
    bridge_ip: Ipv4Addr,
    advertise_ip: Ipv4Addr,
    port_offset: u16,
    image_source: Option<&'a ImageSource>,
    vcpu: Option<u32>,
    memory_gib: Option<u32>,
    disk_gib: Option<u32>,
    no_tee: bool,
}

enum MemberDiskSource<'a> {
    /// Joining validator: local keys directory + validator EVM address + published ports.
    /// Built in a private tempdir and streamed over stdin tar to `disk prepare --source -`.
    JoiningValidator {
        local_keys_dir: &'a Path,
        validator_addr: Address,
        ports: FounderPublishedPorts,
    },
    /// Keyless full node: zero provisioned keys locally, in transit, or on disk (Reth generates
    /// its own ephemeral P2P key inside `--datadir`).
    FullNodeKeyless,
}

struct ProvisionedMember {
    build: NetworkArtifacts,
    member: DesiredMember,
    all_members: Vec<DesiredMember>,
    target_hkey: String,
    target_bin: RemoteOutbeUp,
    outbe_up_sha256: String,
    vm_id: String,
}

fn provision_member(
    req: &ProvisionRequest<'_>,
    disk_source: &MemberDiskSource<'_>,
    exec: &dyn HostExec,
    progress: &mut Progress,
) -> Result<ProvisionedMember> {
    let role_label = match req.role {
        MemberRole::JoiningValidator => "joining-validator",
        MemberRole::FullNode => "keyless full-node",
        MemberRole::Founder => "founder",
    };
    progress.stage(&format!(
        "validating {role_label} `{}` configuration and network state",
        req.name
    ));

    let build = NetworkArtifacts::load(req.artifacts)?;
    ensure_vm_deployment(&build)?;
    let vm_defaults = build
        .config
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("network config has no vm section"))?;

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
    let state_lock = NetworkVmState::lock(req.artifacts)?;
    let mut state = NetworkVmState::load_optional_locked(&state_lock, req.artifacts)?
        .ok_or_else(|| {
            eyre::eyre!(
                "network-vm-state.json not found in {}; deploy the founding network before adding a {role_label}",
                req.artifacts.display()
            )
        })?;
    state.ensure_matches_network(&build)?;
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, Some(&state))?;

    let existing_members = build_all_desired_members(&build, Some(&state), None, None, None)?;
    let vmm_host = resolve_target_ssh(req.host, req.user, req.port, vm_defaults.vmm_host.as_ref())?;
    let vmm_home = req.vmm_home.unwrap_or(&vm_defaults.vmm_home).to_owned();
    let bridge = req.bridge.unwrap_or(&vm_defaults.bridge).to_owned();

    let mut member = build_and_validate_new_member(
        &build,
        &state,
        &existing_members,
        NewMemberSpec {
            role: req.role,
            name: req.name,
            vmm_host,
            vmm_home,
            bridge,
            service_ip: req.bridge_ip,
            advertise_ip: req.advertise_ip,
            port_offset: req.port_offset,
            vcpu: req.vcpu.unwrap_or(vm_defaults.vcpu),
            memory_gib: req.memory_gib.unwrap_or(vm_defaults.memory_gib),
            disk_gib: req.disk_gib.unwrap_or(vm_defaults.disk_gib),
            flavor: vm_defaults.flavor,
            no_tee: req.no_tee,
        },
    )?;

    // 1. Pin outbe-up on target host, verify host readiness, clean stale staging, resolve image.
    progress.stage(&format!(
        "pinning outbe-up, verifying host {}, and resolving guest OS image",
        member.vmm_host.host
    ));
    let target_hkey = host_key(&member.vmm_host, &member.vmm_home);
    let target_bin = ensure_pinned_outbe_up_on_host(
        exec,
        &member.vmm_host,
        &build.root,
        &outbe_up_sha256,
        true,
    )?;
    verify_host_ready_and_clean_stale(
        exec,
        &member.vmm_host,
        &target_bin,
        &member.bridge,
        &member.vmm_home,
    )?;
    let host_img = resolve_image_on_target_host(
        exec,
        &member.vmm_host,
        &target_bin,
        &member.vmm_home,
        req.image_source,
        &state,
    )?;
    member.image_name = Some(host_img.image_name.clone());
    member.image_hash = Some(host_img.os_image_hash.clone());

    // 2. Reconcile target host VM + staged disk (reusing on retry; refusing Foreign/Stale).
    let host_vms = fetch_host_vms(exec, &member.vmm_host, &target_bin, &member.vmm_home)?;
    let host_staged: BTreeMap<String, _> =
        fetch_host_staged_disks(exec, &member.vmm_host, &target_bin)?
            .into_iter()
            .map(|d| (d.staging_name.clone(), d))
            .collect();
    let observed = host_vms.iter().find(|vm| vm.name == member.vm_name);
    let class = classify_host_vm(&member, observed);
    let vm_id = match class {
        VmClassification::Foreign { observed, reason } => {
            bail!(
                "refusing to add `{}` (`{}` on {}, vm_id `{}`): FOREIGN ({reason})",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                observed.vm_id
            );
        }
        VmClassification::Stale { observed, reasons } => {
            bail!(
                "refusing to add `{}` (`{}` on {}, vm_id `{}`): STALE ({}); remove the stale VM first",
                member.name,
                member.vm_name,
                member.vmm_host.host,
                observed.vm_id,
                reasons.join(", ")
            );
        }
        VmClassification::Partial { observed } if observed.phase == "removing" => {
            bail!(
                "VM `{}` on {} is mid-removal; finish `outbe-up vm remove --name {}` first",
                member.vm_name,
                member.vmm_host.host,
                member.vm_name
            );
        }
        VmClassification::Ours { observed } => {
            progress.update(&format!(
                "reusing existing {role_label} VM `{}` ({}) on {}",
                member.vm_name, observed.vm_id, member.vmm_host.host
            ));
            state.upsert_member(build_member_state(
                &member,
                &host_img.os_image_hash,
                observed
                    .disk_source_sha256
                    .clone()
                    .or_else(|| member.recorded_disk_sha256.clone()),
                Some(observed.vm_id.clone()),
                Some(observed.mac.clone()),
            ));
            state.save_locked(&state_lock, req.artifacts)?;
            observed.vm_id
        }
        VmClassification::Absent | VmClassification::Partial { .. } => {
            let partial_required_sha = match &class {
                VmClassification::Partial { observed } => observed.disk_source_sha256.as_deref(),
                _ => None,
            };
            let reusable_sha = partial_required_sha.or(member.recorded_disk_sha256.as_deref());
            let disk_sha256 = if can_reuse_staged_disk(
                host_staged.get(&member.vm_name),
                reusable_sha,
            ) {
                reusable_sha.expect("checked above").to_owned()
            } else {
                let prep = prepare_member_staged_disk(
                    &build,
                    &member,
                    disk_source,
                    &local_op_bundle,
                    &target_bin,
                    exec,
                    progress,
                )?;
                if let Some(expected_sha) = partial_required_sha {
                    ensure!(
                        prep.sha256 == expected_sha,
                        "re-prepared staged disk for partial VM `{}` produced sha256 `{}`, expected `{expected_sha}`",
                        member.vm_name,
                        prep.sha256
                    );
                }
                state.upsert_member(build_member_state(
                    &member,
                    &host_img.os_image_hash,
                    Some(prep.sha256.clone()),
                    state.member(&member.vm_name).and_then(|m| m.vm_id.clone()),
                    state.member(&member.vm_name).and_then(|m| m.mac.clone()),
                ));
                state.save_locked(&state_lock, req.artifacts)?;
                prep.sha256
            };

            progress.stage(&format!(
                "creating stopped {role_label} VM `{}` on {}",
                member.vm_name, member.vmm_host.host
            ));
            let created = create_member_vm_from_staging(exec, &target_bin, &member, &host_img)?;
            state.upsert_member(build_member_state(
                &member,
                &host_img.os_image_hash,
                Some(disk_sha256),
                Some(created.vm_id.clone()),
                Some(created.mac),
            ));
            state.save_locked(&state_lock, req.artifacts)?;
            created.vm_id
        }
    };

    // 3. Mandatory read-only `host check` gate (exit code 2 on failure, BEFORE any funding/start).
    let mut all_members = existing_members;
    if !all_members.iter().any(|m| m.vm_name == member.vm_name) {
        all_members.push(member.clone());
    }
    let publish_ctx = PublishContext::from_members(&all_members);
    if member.role == MemberRole::FullNode {
        ensure!(
            publish_ctx.p2p_port_specs(&member)?.is_empty(),
            "full-node host check must never request inbound published ports"
        );
    }
    progress.stage(&format!(
        "verifying host networking for {role_label} `{}` (`outbe-up host check`)",
        member.vm_name
    ));
    let check_cmd = build_host_check_command(&member, &target_bin, &publish_ctx)?;
    let raw_check = exec
        .capture_raw(&member.vmm_host, &member.name, &check_cmd)
        .wrap_err_with(|| {
            format!(
                "run `outbe-up host check` for `{}` on {}",
                member.vm_name, member.vmm_host.host
            )
        })?;
    let (mut report, rc) = parse_host_check_output(&raw_check, &member.vm_name)?;
    if rc != 0 || !report.service_ip_ok || !report.host_network_ok || !report.published_ports_ok {
        report.ready = false;
    }
    if !report.ready {
        let checks = vec![(member.vmm_host.host.clone(), report)];
        let report_text = format_failed_host_checks(&checks);
        println!(
            "\nHost configuration required before starting `{}` (read-only; zero writes performed):\n{report_text}",
            member.vm_name
        );
        let rerun_cmd = match member.role {
            MemberRole::JoiningValidator => "outbe-net-up network add-validator",
            MemberRole::FullNode => "outbe-net-up network add-full-node",
            MemberRole::Founder => "outbe-net-up network start",
        };
        return Err(eyre::Report::new(ExitCodeError {
            code: EXIT_CODE_HOST_CONFIG_REQUIRED,
            message: format!(
                "host check failed for {role_label} `{}` on {}; apply the host configuration above and re-run `{rerun_cmd}`",
                member.vm_name, member.vmm_host.host
            ),
        }));
    }
    drop(state_lock);

    Ok(ProvisionedMember {
        build,
        member,
        all_members,
        target_hkey,
        target_bin,
        outbe_up_sha256,
        vm_id,
    })
}

fn prepare_member_staged_disk(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    disk_source: &MemberDiskSource<'_>,
    local_op_bundle: &Path,
    target_bin: &RemoteOutbeUp,
    exec: &dyn HostExec,
    progress: &mut Progress,
) -> Result<RemoteDiskPrepareOutput> {
    let op_filename = local_op_bundle
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("outbe-validator-operator-bundle.tar.zst");

    match disk_source {
        MemberDiskSource::JoiningValidator {
            local_keys_dir,
            validator_addr,
            ports,
        } => {
            progress.stage(&format!(
                "streaming joining-validator data disk for `{}` to {} via stdin tar",
                member.name, member.vmm_host.host
            ));
            let tar_bytes = build_joining_validator_tar_bytes(
                build,
                member,
                local_keys_dir,
                *validator_addr,
                *ports,
            )?;
            let remote_dir = make_remote_temp_dir(exec, &member.vmm_host, "outbe-net-up-opbundle")?;
            let remote_bundle = format!("{remote_dir}/{op_filename}");
            let prep_res: Result<RemoteDiskPrepareOutput> = (|| {
                exec.copy_to(
                    &member.vmm_host,
                    &member.vmm_host.host,
                    &[local_op_bundle],
                    &remote_dir,
                    false,
                )
                .wrap_err_with(|| format!("upload operator bundle to {}", member.vmm_host.host))?;
                let prep_cmd = target_bin.cmd(&format!(
                    "disk prepare --role joining-validator --source - --bundle {} --staging-name {} --size-gib {} --json",
                    shell_quote(&remote_bundle),
                    shell_quote(&member.vm_name),
                    member.disk_gib,
                ));
                let raw = exec
                    .capture_stdin(&member.vmm_host, &member.name, &prep_cmd, &tar_bytes)
                    .wrap_err_with(|| {
                        format!(
                            "stream joining-validator data disk for `{}` on {}",
                            member.vm_name, member.vmm_host.host
                        )
                    })?;
                serde_json::from_str(&raw).wrap_err_with(|| {
                    format!(
                        "parse `outbe-up disk prepare --json` output for `{}`",
                        member.vm_name
                    )
                })
            })();
            let _ = cleanup_remote_dir(exec, &member.vmm_host, &remote_dir);
            prep_res
        }
        MemberDiskSource::FullNodeKeyless => {
            progress.stage(&format!(
                "preparing keyless full-node data disk for `{}` on {}",
                member.name, member.vmm_host.host
            ));
            let full_cfg = build_full_node_config(build, member)?;
            let full_yaml = serde_yaml_ng::to_string(&full_cfg)?;
            let local_tmp = tempfile::Builder::new()
                .prefix("outbe-fullnode-cfg-")
                .tempdir()
                .wrap_err("create temp dir for full-node config")?;
            fs::set_permissions(local_tmp.path(), fs::Permissions::from_mode(0o700))?;
            let local_cfg_path = local_tmp.path().join("full-node.yaml");
            fs::write(&local_cfg_path, full_yaml.as_bytes())?;
            fs::set_permissions(&local_cfg_path, fs::Permissions::from_mode(0o600))?;

            let remote_dir = make_remote_temp_dir(exec, &member.vmm_host, "outbe-net-up-fullnode")?;
            let remote_bundle = format!("{remote_dir}/{op_filename}");
            let remote_cfg = format!("{remote_dir}/full-node.yaml");
            let prep_res: Result<RemoteDiskPrepareOutput> = (|| {
                exec.copy_to(
                    &member.vmm_host,
                    &member.vmm_host.host,
                    &[local_op_bundle, local_cfg_path.as_path()],
                    &remote_dir,
                    false,
                )
                .wrap_err_with(|| {
                    format!(
                        "upload operator bundle and full-node config to {}",
                        member.vmm_host.host
                    )
                })?;
                let prep_cmd = target_bin.cmd(&format!(
                    "disk prepare --role full-node --bundle {} --config {} --staging-name {} --size-gib {} --json",
                    shell_quote(&remote_bundle),
                    shell_quote(&remote_cfg),
                    shell_quote(&member.vm_name),
                    member.disk_gib,
                ));
                run_host_json(exec, &member.vmm_host, &member.name, &prep_cmd).wrap_err_with(|| {
                    format!(
                        "prepare keyless full-node data disk for `{}` on {}",
                        member.vm_name, member.vmm_host.host
                    )
                })
            })();
            let _ = cleanup_remote_dir(exec, &member.vmm_host, &remote_dir);
            prep_res
        }
    }
}

fn start_provisioned_vm(
    exec: &dyn HostExec,
    bin: &RemoteOutbeUp,
    member: &DesiredMember,
    vm_id: &str,
    progress: &mut Progress,
) -> Result<()> {
    progress.stage(&format!(
        "starting VM `{}` ({vm_id}) on {}",
        member.vm_name, member.vmm_host.host
    ));
    let start_cmd = bin.cmd(&format!(
        "vm start --name {} --expect-vm-id {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(vm_id),
        shell_quote_tilde_path(&member.vmm_home),
    ));
    let _: RemoteVmStartOutput = run_host_json(exec, &member.vmm_host, &member.name, &start_cmd)
        .wrap_err_with(|| {
            format!(
                "start VM `{}` ({vm_id}) on {}",
                member.vm_name, member.vmm_host.host
            )
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Joining Validator VM orchestration
// ---------------------------------------------------------------------------

pub(crate) async fn add_validator_with_exec(
    options: &AddValidatorOptions<'_>,
    exec: Arc<dyn HostExec>,
    driver: &dyn JoinChainDriver,
) -> Result<()> {
    let deadline = Instant::now() + options.timeout;
    let mut progress = Progress::new();

    // Generate or reuse joining validator keys locally before provisioning the disk.
    // Fails closed if the member keys directory is partially populated (F5).
    let build_probe = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build_probe)?;
    let ports = FounderPublishedPorts::from_offset(options.port_offset)?;
    progress.stage(&format!(
        "ensuring local joining-validator keys for `{}`",
        options.name
    ));
    let local_keys_dir =
        ensure_local_member_keys(&build_probe.root, options.name, build_probe.config.chain_id)?;
    let validator_addr = validator_address(&local_keys_dir.join("evm-key.hex"))?;
    let feeder_addr = validator_address(&local_keys_dir.join("feeder-key.hex"))?;

    let provisioned = provision_member(
        &ProvisionRequest {
            artifacts: options.artifacts,
            role: MemberRole::JoiningValidator,
            name: options.name,
            host: options.host,
            user: options.user,
            port: options.port,
            vmm_home: options.vmm_home,
            bridge: options.bridge,
            bridge_ip: options.bridge_ip,
            advertise_ip: options.advertise_ip,
            port_offset: options.port_offset,
            image_source: options.image_source.as_ref(),
            vcpu: options.vcpu,
            memory_gib: options.memory_gib,
            disk_gib: options.disk_gib,
            no_tee: options.no_tee,
        },
        &MemberDiskSource::JoiningValidator {
            local_keys_dir: &local_keys_dir,
            validator_addr,
            ports,
        },
        exec.as_ref(),
        &mut progress,
    )?;

    let ProvisionedMember {
        build,
        member,
        all_members,
        target_hkey,
        target_bin,
        outbe_up_sha256,
        vm_id,
    } = provisioned;

    // F1 + F7: Fund the joining validator BEFORE `vm start` with `min_stake + FEEDER_FUNDING_WEI +
    // JOINER_GAS_RESERVE_WEI`. `registerEnclave` is a paid transaction (not ZeroFee), so the node
    // cannot obtain its resident TEE offer key (`offer_key_ready`) while its balance is zero.
    let founder0_key = build
        .root
        .join("secrets/founder-keys/validator-0/evm-key.hex");
    let min_stake = driver.min_stake().await?;
    let mut record = driver.validator(validator_addr).await?;
    if !record.registered {
        let required_balance = min_stake
            .checked_add(U256::from(FEEDER_FUNDING_WEI))
            .and_then(|v| v.checked_add(U256::from(JOINER_GAS_RESERVE_WEI)))
            .ok_or_else(|| eyre::eyre!("required joiner balance overflow"))?;
        let current_balance = driver.balance(validator_addr).await?;
        if current_balance < required_balance {
            ensure!(
                founder0_key.is_file(),
                "founder-0 key missing at {} (cannot fund joiner {validator_addr:#x})",
                founder0_key.display()
            );
            let topup = required_balance - current_balance;
            progress.stage(&format!(
                "funding joining validator `{}` ({validator_addr:#x}) before VM start with {topup} wei (min_stake + feeder + gas reserve)",
                member.name
            ));
            let tx = driver
                .fund_address(&founder0_key, validator_addr, topup)
                .await?;
            driver.wait_tx_finalized(&tx, deadline).await?;
            ensure!(
                driver.balance(validator_addr).await? >= required_balance,
                "joining validator {validator_addr:#x} funding not visible after finality"
            );
        }
    }

    // Start VM with `--expect-vm-id`.
    start_provisioned_vm(exec.as_ref(), &target_bin, &member, &vm_id, &mut progress)?;

    // Resolve pinned `outbe-up` on all active validator hosts for status/lockstep verification.
    let active_validators: Vec<DesiredMember> = all_members
        .iter()
        .filter(|m| m.role != MemberRole::FullNode)
        .cloned()
        .collect();
    let unique_val_hosts = collect_unique_hosts(&active_validators);
    let mut val_host_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    val_host_bins.insert(target_hkey, target_bin.clone());
    for (hkey, host) in &unique_val_hosts {
        if val_host_bins.contains_key(hkey) {
            continue;
        }
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            false,
        )?;
        val_host_bins.insert(hkey.clone(), bin);
    }

    // Wait for joiner RPC + resident TEE (`registerEnclave` paid from pre-funded balance) + sync.
    progress.stage(&format!(
        "waiting for `{}` RPC, resident TEE, and initial sync",
        member.name
    ));
    wait_for_joiner_synced(
        &build,
        &member,
        &unique_val_hosts,
        &val_host_bins,
        &exec,
        driver,
        options.max_lag,
        deadline,
        &mut progress,
    )
    .await?;

    // Bond minimum stake -> PENDING.
    record = driver.validator(validator_addr).await?;
    if !record.registered {
        let symmetric = format!("{}:{}", member.advertise_ip, ports.consensus_p2p_tcp);
        progress.stage(&format!(
            "bonding minimum stake ({min_stake} wei) for `{}` ({validator_addr:#x}, p2p {symmetric})",
            member.name
        ));
        let tx = driver.stake(&local_keys_dir, validator_addr, min_stake, &symmetric)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        record = driver.validator(validator_addr).await?;
        ensure!(
            record.registered && matches!(record.status, 1 | 2),
            "joining validator {validator_addr:#x} not PENDING/ACTIVE after stake finality (registered={}, status={})",
            record.registered,
            record.status
        );
    }

    // Delegate oracle feeder & fund feeder key (Hyperlane excluded from VM roles until S9).
    let joiner_evm_key = local_keys_dir.join("evm-key.hex");
    if driver
        .validator_delegate(validator_addr, ORACLE_DELEGATE_ROLE)
        .await?
        != feeder_addr
    {
        progress.stage(&format!(
            "delegating oracle role for `{}` to feeder {feeder_addr:#x}",
            member.name
        ));
        let tx = driver.delegate_oracle(&joiner_evm_key, feeder_addr)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        ensure!(
            driver
                .validator_delegate(validator_addr, ORACLE_DELEGATE_ROLE)
                .await?
                == feeder_addr,
            "oracle feeder delegation not visible after finality"
        );
    }
    if driver.balance(feeder_addr).await?.is_zero() {
        progress.stage(&format!(
            "funding oracle feeder {feeder_addr:#x} for `{}`",
            member.name
        ));
        let tx = driver
            .fund_address(&joiner_evm_key, feeder_addr, U256::from(FEEDER_FUNDING_WEI))
            .await?;
        driver.wait_tx_finalized(&tx, deadline).await?;
        ensure!(
            !driver.balance(feeder_addr).await?.is_zero(),
            "oracle feeder funding not visible after finality"
        );
    }

    // Wait for joiner `verifier` (or `signer`) mode & sync, then send `confirm-ready`.
    progress.stage(&format!(
        "waiting for `{}` to reach verifier mode before confirm-ready",
        member.name
    ));
    wait_for_joiner_verifier(
        &build,
        &member,
        &unique_val_hosts,
        &val_host_bins,
        &exec,
        driver,
        options.max_lag,
        deadline,
        &mut progress,
    )
    .await?;

    record = driver.validator(validator_addr).await?;
    if record.status == 1 {
        progress.stage(&format!(
            "sending validator confirm-ready for `{}` ({validator_addr:#x})",
            member.name
        ));
        let tx = driver.confirm_ready(&joiner_evm_key)?;
        driver.wait_tx_finalized(&tx, deadline).await?;
    }

    // Wait for DKG reshare -> ACTIVE + has_bls_share + signer + has_threshold_shares +
    // post-activation lockstep >= min_finalized_blocks across 2 consecutive samples with
    // identical block hash against founder-0.
    progress.stage(&format!(
        "waiting for DKG reshare activation and +{} post-activation finalized blocks for `{}`",
        options.min_finalized_blocks, member.name
    ));
    let required_peers = active_validators.len().saturating_sub(1);
    let founder0 = active_validators
        .first()
        .ok_or_else(|| eyre::eyre!("no founding validator found"))?;
    let mut activation_baseline: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let onchain = driver.validator(validator_addr).await?;
        let (statuses, _) = query_members_and_orphans(
            &build,
            &active_validators,
            &unique_val_hosts,
            &val_host_bins,
            &exec,
        )
        .await?;
        let assessments = super::assess_members_lag(&statuses, options.max_lag);

        let joiner_status = statuses.iter().find(|s| s.vm_name == member.vm_name);
        let founder0_status = statuses.iter().find(|s| s.vm_name == founder0.vm_name);

        let activated = onchain.registered
            && onchain.status == 2
            && onchain.has_bls_share
            && joiner_status.is_some_and(|s| {
                s.classification == VmClassKind::Ours
                    && member_node_ready(s, build.config.chain_id, required_peers)
            });

        if activated && activation_baseline.is_none() {
            activation_baseline = super::compute_readiness_baseline(&statuses);
        }

        let mut sample_ready = false;
        if activated {
            if let Some(b) = activation_baseline {
                let target = b.saturating_add(options.min_finalized_blocks);
                let all_validators_ok = statuses.iter().zip(&assessments).all(|(s, a)| {
                    s.classification == VmClassKind::Ours
                        && member_node_ready(s, build.config.chain_id, required_peers)
                        && !a.lagging
                        && s.finalized.is_some_and(|h| h >= target)
                });
                let vrf_versions: BTreeSet<u64> = statuses
                    .iter()
                    .filter_map(|s| s.vrf_material_version)
                    .collect();
                if all_validators_ok && vrf_versions.len() == 1 {
                    if let (Some(js), Some(f0)) = (joiner_status, founder0_status) {
                        if verify_finalized_hash_match(exec.as_ref(), &member, js, founder0, f0)? {
                            sample_ready = true;
                        }
                    }
                }
            }
        }

        if sample_ready {
            consecutive_ready += 1;
            if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
                progress.done(&format!(
                    "joining validator `{}` ({validator_addr:#x}) is ACTIVE signer in lockstep (baseline B={}, growth >= +{})",
                    member.name,
                    activation_baseline.unwrap_or(0),
                    options.min_finalized_blocks,
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
            let tail = fetch_member_serial_tail(exec.as_ref(), &member, &vm_id, &target_bin);
            bail!(
                "timed out after {}s waiting for joining validator `{}` ({validator_addr:#x}) activation & lockstep (onchain status={}, has_bls_share={}, baseline={:?}):\n\n{}{}",
                options.timeout.as_secs(),
                member.name,
                onchain.status,
                onchain.has_bls_share,
                activation_baseline,
                render_vm_status(&build, &statuses, &[], options.max_lag),
                tail,
            );
        }

        progress.update(&format!(
            "onchain_status={} bls_share={} joiner_mode={} baseline={:?} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            onchain.status,
            onchain.has_bls_share,
            joiner_status
                .and_then(|s| s.consensus_mode.as_deref())
                .unwrap_or("-"),
            activation_baseline,
        ));
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Keyless Full Node VM orchestration
// ---------------------------------------------------------------------------

/// Provision and start a keyless full-node VM (zero provisioned keys; Reth generates its own
/// ephemeral P2P key inside `--datadir`) and verify it tracks founder-0 finalized block hashes.
pub(crate) async fn add_full_node_with_exec(
    options: &AddFullNodeOptions<'_>,
    exec: Arc<dyn HostExec>,
) -> Result<()> {
    let deadline = Instant::now() + options.timeout;
    let mut progress = Progress::new();
    let advertise_ip = options.advertise_ip.unwrap_or(options.bridge_ip);

    let provisioned = provision_member(
        &ProvisionRequest {
            artifacts: options.artifacts,
            role: MemberRole::FullNode,
            name: options.name,
            host: options.host,
            user: options.user,
            port: options.port,
            vmm_home: options.vmm_home,
            bridge: options.bridge,
            bridge_ip: options.bridge_ip,
            advertise_ip,
            port_offset: 0,
            image_source: options.image_source.as_ref(),
            vcpu: options.vcpu,
            memory_gib: options.memory_gib,
            disk_gib: options.disk_gib,
            no_tee: options.no_tee,
        },
        &MemberDiskSource::FullNodeKeyless,
        exec.as_ref(),
        &mut progress,
    )?;

    let ProvisionedMember {
        build,
        member,
        all_members,
        target_hkey,
        target_bin,
        outbe_up_sha256,
        vm_id,
    } = provisioned;

    start_provisioned_vm(exec.as_ref(), &target_bin, &member, &vm_id, &mut progress)?;

    // Verify full node tracks founder-0 finalized height across >= min_finalized_blocks with
    // identical block hashes (`eth_getBlockByNumber`) across 2 consecutive samples.
    let founder0 = all_members
        .iter()
        .find(|m| m.role == MemberRole::Founder)
        .cloned()
        .ok_or_else(|| eyre::eyre!("no founding validator found"))?;
    let pair = vec![founder0.clone(), member.clone()];
    let unique_pair_hosts = collect_unique_hosts(&pair);
    let mut pair_bins: BTreeMap<String, RemoteOutbeUp> = BTreeMap::new();
    pair_bins.insert(target_hkey, target_bin.clone());
    for (hkey, host) in &unique_pair_hosts {
        if pair_bins.contains_key(hkey) {
            continue;
        }
        let bin = ensure_pinned_outbe_up_on_host(
            exec.as_ref(),
            &host.ssh,
            &build.root,
            &outbe_up_sha256,
            false,
        )?;
        pair_bins.insert(hkey.clone(), bin);
    }

    progress.stage(&format!(
        "verifying keyless full node `{}` tracks finalized height (+{} blocks, matching hashes)",
        member.name, options.min_finalized_blocks
    ));
    let mut baseline_b: Option<u64> = None;
    let mut consecutive_ready: u32 = 0;

    loop {
        let (statuses, _) =
            query_members_and_orphans(&build, &pair, &unique_pair_hosts, &pair_bins, &exec).await?;
        let f0_status = statuses.iter().find(|s| s.vm_name == founder0.vm_name);
        let fn_status = statuses.iter().find(|s| s.vm_name == member.vm_name);

        if baseline_b.is_none() {
            baseline_b = super::compute_readiness_baseline(&statuses);
        }

        let mut sample_ready = false;
        if let (Some(b), Some(f0), Some(fns)) = (baseline_b, f0_status, fn_status) {
            let target = b.saturating_add(options.min_finalized_blocks);
            if fns.classification == VmClassKind::Ours
                && member_node_ready(fns, build.config.chain_id, 0)
            {
                if let (Some(f0_h), Some(fn_h)) = (f0.finalized, fns.finalized) {
                    let lag = f0_h.saturating_sub(fn_h);
                    if fn_h >= target
                        && lag <= options.max_lag
                        && verify_finalized_hash_match(exec.as_ref(), &member, fns, &founder0, f0)?
                    {
                        sample_ready = true;
                    }
                }
            }
        }

        if sample_ready {
            consecutive_ready += 1;
            if consecutive_ready >= READY_CONSECUTIVE_SAMPLES {
                progress.done(&format!(
                    "keyless full node `{}` synced and tracking finalized blocks (baseline B={}, growth >= +{})",
                    member.name,
                    baseline_b.unwrap_or(0),
                    options.min_finalized_blocks,
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
            let tail = fetch_member_serial_tail(exec.as_ref(), &member, &vm_id, &target_bin);
            bail!(
                "timed out after {}s waiting for keyless full node `{}` to track finalized blocks (baseline={:?}, required=+{}):\n\n{}{}",
                options.timeout.as_secs(),
                member.name,
                baseline_b,
                options.min_finalized_blocks,
                render_vm_status(&build, &statuses, &[], options.max_lag),
                tail,
            );
        }

        progress.update(&format!(
            "full_node_finalized={:?} founder0_finalized={:?} baseline={:?} ready_streak={consecutive_ready}/{READY_CONSECUTIVE_SAMPLES}",
            fn_status.and_then(|s| s.finalized),
            f0_status.and_then(|s| s.finalized),
            baseline_b,
        ));
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers for add-validator and add-full-node
// ---------------------------------------------------------------------------

struct NewMemberSpec<'a> {
    role: MemberRole,
    name: &'a str,
    vmm_host: Ssh,
    vmm_home: String,
    bridge: String,
    service_ip: Ipv4Addr,
    advertise_ip: Ipv4Addr,
    port_offset: u16,
    vcpu: u32,
    memory_gib: u32,
    disk_gib: u32,
    flavor: VmFlavor,
    no_tee: bool,
}

fn build_and_validate_new_member(
    build: &NetworkArtifacts,
    state: &NetworkVmState,
    existing_members: &[DesiredMember],
    spec: NewMemberSpec<'_>,
) -> Result<DesiredMember> {
    ensure!(
        !spec.name.is_empty()
            && spec
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "invalid member name `{}` (expected letters, digits, `-`, or `_`)",
        spec.name
    );
    ensure!(
        !build.config.founders.iter().any(|f| f.name == spec.name),
        "member name `{}` collides with a founding validator",
        spec.name
    );
    let vm_name = founder_vm_name(&build.config.name, spec.name);
    ensure!(
        !vm_name.is_empty() && vm_name.len() <= 63,
        "computed VM name `{vm_name}` exceeds 63 characters"
    );
    ensure!(
        vm_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric()),
        "computed VM name `{vm_name}` must start with an ASCII letter or digit"
    );
    ensure!(spec.vcpu > 0, "vcpu must be positive");
    ensure!(spec.memory_gib > 0, "memory_gib must be positive");
    ensure!(spec.disk_gib > 0, "disk_gib must be positive");
    ensure!(
        spec.service_ip.is_private() && !spec.service_ip.is_loopback(),
        "bridge_ip ({}) must be a non-loopback private IPv4 address",
        spec.service_ip
    );

    let vm_defaults = build
        .config
        .vm
        .as_ref()
        .ok_or_else(|| eyre::eyre!("network config has no vm section"))?;
    let prefix = vm_defaults
        .prefix
        .ok_or_else(|| eyre::eyre!("missing vm.prefix in network config"))?;
    let gateway = vm_defaults
        .gateway
        .ok_or_else(|| eyre::eyre!("missing vm.gateway in network config"))?;
    StaticVmNetworkConfig {
        address: spec.service_ip,
        prefix_len: prefix,
        gateway,
        dns: vm_defaults.dns.clone(),
    }
    .validate()
    .wrap_err_with(|| format!("invalid static VM addressing for `{}`", spec.name))?;

    // Check collisions against other members (allow exact same vm_name on idempotent retry).
    for other in existing_members {
        if other.vm_name == vm_name {
            ensure!(
                other.role == spec.role,
                "member `{}` already exists with role {:?}, cannot reuse as {:?}",
                spec.name,
                other.role,
                spec.role
            );
            continue;
        }
        ensure!(
            !(other.vmm_host.host == spec.vmm_host.host && other.service_ip == spec.service_ip),
            "bridge_ip {} on host {} collides with existing member `{}`",
            spec.service_ip,
            spec.vmm_host.host,
            other.name
        );
        if spec.role == MemberRole::JoiningValidator && other.role != MemberRole::FullNode {
            let new_ports = FounderPublishedPorts::from_offset(spec.port_offset)?;
            let other_ports = FounderPublishedPorts::from_offset(other.port_offset)?;
            for (svc, proto, port) in new_ports.bindings() {
                for (osvc, oproto, oport) in other_ports.bindings() {
                    if proto == oproto && port == oport {
                        ensure!(
                            other.vmm_host.host != spec.vmm_host.host,
                            "port collision on host {}: `{}` ({svc}/{proto}:{port}) collides with `{}` ({osvc}/{oproto}:{oport}); pass a distinct --port-offset",
                            spec.vmm_host.host,
                            spec.name,
                            other.name
                        );
                        ensure!(
                            other.advertise_ip != spec.advertise_ip,
                            "advertised port collision on {}: `{}` ({svc}/{proto}:{port}) collides with `{}` ({osvc}/{oproto}:{oport})",
                            spec.advertise_ip,
                            spec.name,
                            other.name
                        );
                    }
                }
            }
        }
    }

    let prev = state.member(&vm_name);
    let next_index = prev.map(|m| m.index).unwrap_or_else(|| {
        existing_members
            .iter()
            .map(|m| m.index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(build.config.founders.len())
    });

    Ok(DesiredMember {
        index: next_index,
        role: spec.role,
        name: spec.name.to_owned(),
        vm_name,
        owner_tag: member_owner_tag(
            &build.config.name,
            &build.manifest.genesis.final_sha256,
            spec.name,
        ),
        vmm_host: spec.vmm_host,
        vmm_home: spec.vmm_home,
        bridge: spec.bridge,
        service_ip: spec.service_ip,
        advertise_ip: spec.advertise_ip,
        port_offset: spec.port_offset,
        vcpu: spec.vcpu,
        memory_gib: spec.memory_gib,
        disk_gib: spec.disk_gib,
        flavor: spec.flavor,
        no_tee: spec.no_tee,
        image_name: None,
        image_hash: None,
        recorded_disk_sha256: prev.and_then(|m| m.disk_source_sha256.clone()),
    })
}

fn resolve_target_ssh(
    host: &str,
    user: Option<&str>,
    port: u16,
    default_ssh: Option<&Ssh>,
) -> Result<Ssh> {
    let ssh = Ssh {
        host: host.to_owned(),
        user: user
            .map(ToOwned::to_owned)
            .or_else(|| default_ssh.map(|s| s.user.clone()))
            .unwrap_or_else(|| "ubuntu".to_owned()),
        port,
    };
    validate_ssh(&ssh, host)?;
    Ok(ssh)
}

fn verify_host_ready_and_clean_stale(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    bridge: &str,
    vmm_home: &str,
) -> Result<()> {
    let status_cmd = bin.cmd(&format!(
        "host status --bridge {} --vmm-home {} --json",
        shell_quote(bridge),
        shell_quote_tilde_path(vmm_home),
    ));
    let host_status: RemoteHostStatusOutput = run_host_json(exec, ssh, &ssh.host, &status_cmd)
        .wrap_err_with(|| {
            format!(
                "inspect host readiness via `outbe-up host status` on {}",
                ssh.host
            )
        })?;
    ensure!(
        host_status.ready_for_vm,
        "VMM host {} is not ready for VMs (`outbe-up host status` reported ready_for_vm=false): {}",
        ssh.host,
        if host_status.notes.is_empty() {
            format!(
                "run `outbe-up host status --bridge {bridge}` on {} for details",
                ssh.host
            )
        } else {
            host_status.notes.join("; ")
        }
    );
    let stale_cmd = bin.cmd("disk clean --stale --json");
    let _: RemoteCleanStaleOutput = run_host_json(exec, ssh, &ssh.host, &stale_cmd)
        .wrap_err_with(|| format!("clean stale staging dirs on {}", ssh.host))?;
    Ok(())
}

fn resolve_image_on_target_host(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    vmm_home: &str,
    image_source: Option<&ImageSource>,
    state: &NetworkVmState,
) -> Result<RemoteImagePublishOutput> {
    if let Some(src) = image_source {
        let (resolved, _) = resolve_image_source(src)?;
        return match resolved {
            ResolvedImageSource::Archive { path, sha256 } => {
                import_image_on_host(exec, ssh, bin, vmm_home, &path, &sha256)
            }
            ResolvedImageSource::Url { url, sha256 } => {
                fetch_image_on_host(exec, ssh, bin, vmm_home, &url, &sha256)
            }
        };
    }

    ensure!(
        !state.image_hash.is_empty(),
        "network-vm-state.json has no recorded guest OS image hash; pass `--image-archive` or `--image-url --image-sha256`"
    );
    let list_cmd = bin.cmd(&format!(
        "image list --vmm-home {} --json",
        shell_quote_tilde_path(vmm_home),
    ));
    let listed: Vec<RemoteListedImage> = run_host_json(exec, ssh, &ssh.host, &list_cmd)
        .wrap_err_with(|| format!("list guest OS images on {}", ssh.host))?;
    let found = listed
        .into_iter()
        .find(|img| img.has_archive_manifest && img.os_image_hash == state.image_hash)
        .ok_or_else(|| {
            eyre::eyre!(
                "guest OS image `{}` ({}) is not present on {}; pass `--image-archive` or `--image-url --image-sha256`",
                state.image_name,
                state.image_hash,
                ssh.host
            )
        })?;
    Ok(RemoteImagePublishOutput {
        image_name: found.name,
        os_image_hash: found.os_image_hash,
        target_dir: found.path,
        reused: true,
    })
}

/// Ensure member keys exist under `<artifacts>/secrets/members/<name>/` (`0700`/`0600`).
///
/// Never deletes an existing non-empty member keys directory (F5): if all required keys are
/// present they are reused; if the directory is partially populated, fails closed naming the
/// missing file(s) so an already-staked identity can never be silently overwritten.
pub(crate) fn ensure_local_member_keys(
    artifacts: &Path,
    name: &str,
    chain_id: u64,
) -> Result<PathBuf> {
    let secrets_members = artifacts.join("secrets/members");
    fs::create_dir_all(&secrets_members)?;
    fs::set_permissions(&secrets_members, fs::Permissions::from_mode(0o700))?;
    let dir = secrets_members.join(name);

    match fs::symlink_metadata(&dir) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_dir() && !meta.file_type().is_symlink(),
                "refusing non-directory member keys path: {}",
                dir.display()
            );
            let missing: Vec<&str> = REQUIRED_MEMBER_KEYS
                .iter()
                .copied()
                .filter(|rel| !dir.join(rel).is_file())
                .collect();
            if missing.is_empty() {
                normalize_local_keys(&dir)?;
                return Ok(dir);
            }
            let is_empty = fs::read_dir(&dir)?.next().is_none();
            ensure!(
                is_empty,
                "refusing to regenerate keys into partial member keys directory {} (missing required key file(s): {}); restore the missing file(s) or remove the directory manually if no stake/VM exists",
                dir.display(),
                missing.join(", ")
            );
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&dir)?;
        }
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("stat member keys dir {}", dir.display()));
        }
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;

    let bundle = artifacts.join("software-bundle.tar.zst");
    let tmp = tempfile::Builder::new()
        .prefix("outbe-keygen-member-")
        .tempdir()
        .wrap_err("create temp dir for bundled outbe-keygen")?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
    verify_and_extract(&bundle, tmp.path())
        .wrap_err("verify software bundle for outbe-keygen extraction")?;
    let keygen = tmp.path().join("bin/outbe-keygen");
    ensure!(
        keygen.is_file(),
        "software bundle {} is missing bin/outbe-keygen",
        bundle.display()
    );
    let dir_str = dir
        .to_str()
        .ok_or_else(|| eyre::eyre!("non-UTF-8 member keys path: {}", dir.display()))?;
    // Suppress stdout/stderr so keygen output is never buffered or logged.
    let status = Command::new(&keygen)
        .args([
            "validator",
            "--output-dir",
            dir_str,
            "--chain-id",
            &chain_id.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .wrap_err_with(|| format!("execute {}", keygen.display()))?;
    ensure!(
        status.success(),
        "outbe-keygen validator failed with {status}"
    );
    normalize_local_keys(&dir)?;
    Ok(dir)
}

fn normalize_local_keys(keys_dir: &Path) -> Result<()> {
    fs::set_permissions(keys_dir, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(keys_dir).follow_links(false) {
        let entry = entry?;
        let mode = if entry.file_type().is_dir() {
            0o700
        } else {
            0o600
        };
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
    }
    let p2p_key = keys_dir.join("reth-p2p-secret.hex");
    if p2p_key.is_file() {
        let normalized: String = fs::read_to_string(&p2p_key)?
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        fs::write(&p2p_key, normalized.as_bytes())?;
        fs::set_permissions(&p2p_key, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn build_joining_validator_tar_bytes(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    local_keys_dir: &Path,
    validator_addr: Address,
    ports: FounderPublishedPorts,
) -> Result<Vec<u8>> {
    let example_path = build.root.join(&build.manifest.joining.config_example);
    let raw_example = fs::read_to_string(&example_path).wrap_err_with(|| {
        format!(
            "read joining operator config template at {}",
            example_path.display()
        )
    })?;
    // Parse the template before overriding placeholder `rpc_bind_ip`/`advertise_ip` (the template
    // uses `127.0.0.1` placeholder alongside `vm_prefix`/`vm_gateway`), then validate strictly.
    // Hyperlane configuration is left as-is from the template (Hyperlane is not started on VMs
    // until S9).
    let mut op_cfg: OperatorConfig = serde_yaml_ng::from_str(&raw_example).wrap_err_with(|| {
        format!(
            "invalid joining operator config template at {}",
            example_path.display()
        )
    })?;
    op_cfg.network.advertise_ip = member.advertise_ip;
    op_cfg.network.rpc_bind_ip = member.service_ip;
    op_cfg.network.vm_consensus_port = Some(ports.consensus_p2p_tcp);
    op_cfg.network.vm_reth_port = Some(ports.reth_p2p_tcp);
    op_cfg.network.vm_discv5_port = Some(ports.reth_discv5_udp);
    op_cfg.network.vm_radicle_port = Some(ports.radicle_tcp);
    op_cfg.validate()?;

    let tmp = tempfile::Builder::new()
        .prefix("outbe-join-tree-")
        .tempdir()
        .wrap_err("create temp dir for joining validator tree")?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
    let tree = tmp.path().join("tree");
    fs::create_dir_all(&tree)?;
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o755))?;

    let genesis_src = build.root.join(&build.manifest.genesis.final_path);
    copy_file_mode(&genesis_src, &tree.join("genesis.json"), 0o644)?;
    let crs_src = find_crs_file(&build.root)?;
    copy_file_mode(&crs_src, &tree.join(".bb-crs/bn254_g1.dat"), 0o644)?;

    let dst_keys = tree.join("keys");
    copy_keys_tree(local_keys_dir, &dst_keys)?;

    op_cfg.write_runtime_config_for_role(&tree, validator_addr, NodeRole::JoiningValidator)?;
    for name in [
        "validator.env",
        "feeder-public.toml",
        "hyperlane-public.toml",
    ] {
        fs::set_permissions(tree.join(name), fs::Permissions::from_mode(0o640))?;
    }

    build_founder_tar_bytes(&tree)
}

fn build_full_node_config(
    build: &NetworkArtifacts,
    member: &DesiredMember,
) -> Result<FullNodeConfig> {
    let example_path = build.root.join(&build.manifest.joining.config_example);
    let raw_example = fs::read_to_string(&example_path).wrap_err_with(|| {
        format!(
            "read joining operator config template at {}",
            example_path.display()
        )
    })?;
    let op_cfg: OperatorConfig = serde_yaml_ng::from_str(&raw_example).wrap_err_with(|| {
        format!(
            "invalid joining operator config template at {}",
            example_path.display()
        )
    })?;
    let cfg = FullNodeConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: FullNodeNetworkConfig {
            chain_id: build.config.chain_id,
            certified_rpc: build.config.certified_rpc.clone(),
            advertise_ip: member.advertise_ip,
            rpc_bind_ip: member.service_ip,
            vm_prefix: op_cfg.network.vm_prefix,
            vm_gateway: op_cfg.network.vm_gateway,
            vm_dns: op_cfg.network.vm_dns,
            vm_consensus_port: Some(DEFAULT_CONSENSUS_PORT),
            vm_reth_port: Some(DEFAULT_RETH_PORT),
            vm_discv5_port: Some(DEFAULT_DISCV5_PORT),
            vm_radicle_port: Some(DEFAULT_RADICLE_PORT),
            bootnodes: op_cfg.network.bootnodes,
            use_local_defaults: op_cfg.network.use_local_defaults,
        },
    };
    cfg.validate()?;
    Ok(cfg)
}

fn create_member_vm_from_staging(
    exec: &dyn HostExec,
    bin: &RemoteOutbeUp,
    member: &DesiredMember,
    host_img: &RemoteImagePublishOutput,
) -> Result<RemoteVmCreateOutput> {
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
        run_host_json(exec, &member.vmm_host, &member.name, &create_cmd).wrap_err_with(|| {
            format!("create VM `{}` on {}", member.vm_name, member.vmm_host.host)
        })?;
    ensure!(
        created.name == member.vm_name
            && created.image_name == host_img.image_name
            && created.os_image_hash == host_img.os_image_hash,
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
            exec,
            &member.vmm_host,
            &member.name,
            &clean_cmd,
        );
    }
    Ok(created)
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_joiner_synced(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    unique_hosts: &BTreeMap<String, super::UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &Arc<dyn HostExec>,
    driver: &dyn JoinChainDriver,
    max_lag: u64,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let target_slice = vec![member.clone()];
    loop {
        let tip = driver.finalized_block().await.ok().map(|b| b.number);
        let (statuses, _) =
            query_members_and_orphans(build, &target_slice, unique_hosts, host_bins, exec).await?;
        if let Some(s) = statuses.first() {
            let tee_ok = s.no_tee
                || s.consensus
                    .as_ref()
                    .is_some_and(outbe_operator_node_role::rpc::ConsensusStatus::resident_ready);
            let chain_ok = s.chain_id == Some(build.config.chain_id);
            let synced = match (s.finalized, tip) {
                (Some(mine), Some(remote)) => remote.saturating_sub(mine) <= max_lag,
                _ => false,
            };
            if s.vmm_running && s.error.is_none() && chain_ok && tee_ok && synced {
                return Ok(());
            }
            progress.update(&format!(
                "joiner `{}` boot/sync: finalized={:?} tip={:?} mode={:?} tee_ok={tee_ok}",
                member.name, s.finalized, tip, s.consensus_mode
            ));
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for joining validator `{}` RPC, resident TEE, and initial sync",
            member.name
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_joiner_verifier(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    unique_hosts: &BTreeMap<String, super::UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &Arc<dyn HostExec>,
    driver: &dyn JoinChainDriver,
    max_lag: u64,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<()> {
    let target_slice = vec![member.clone()];
    loop {
        let tip = driver.finalized_block().await.ok().map(|b| b.number);
        let (statuses, _) =
            query_members_and_orphans(build, &target_slice, unique_hosts, host_bins, exec).await?;
        if let Some(s) = statuses.first() {
            let mode_ok = matches!(s.consensus_mode.as_deref(), Some("verifier" | "signer"));
            let tee_ok = s.no_tee
                || s.consensus
                    .as_ref()
                    .is_some_and(outbe_operator_node_role::rpc::ConsensusStatus::resident_ready);
            let synced = match (s.finalized, tip) {
                (Some(mine), Some(remote)) => remote.saturating_sub(mine) <= max_lag,
                _ => false,
            };
            if s.vmm_running && s.error.is_none() && mode_ok && tee_ok && synced {
                return Ok(());
            }
            progress.update(&format!(
                "joiner `{}` waiting for verifier: mode={:?} finalized={:?} tip={:?} tee_ok={tee_ok}",
                member.name, s.consensus_mode, s.finalized, tip
            ));
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for joining validator `{}` to enter verifier mode",
            member.name
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
    }
}

/// Verify two VMs agree on the exact canonical block hash at `min(a.finalized, b.finalized)`.
pub(super) fn verify_finalized_hash_match(
    exec: &dyn HostExec,
    member_a: &DesiredMember,
    status_a: &VmMemberStatus,
    member_b: &DesiredMember,
    status_b: &VmMemberStatus,
) -> Result<bool> {
    let (Some(ha), Some(hb)) = (status_a.finalized, status_b.finalized) else {
        return Ok(false);
    };
    if ha == hb {
        if let (Some(hash_a), Some(hash_b)) = (
            status_a.finalized_hash.as_deref(),
            status_b.finalized_hash.as_deref(),
        ) {
            return Ok(hash_a == hash_b);
        }
    }
    let check_height = ha.min(hb);
    let hash_a = query_vm_block_hash(
        exec,
        &member_a.vmm_host,
        &member_a.name,
        status_a.service_ip,
        check_height,
    )?;
    let hash_b = query_vm_block_hash(
        exec,
        &member_b.vmm_host,
        &member_b.name,
        status_b.service_ip,
        check_height,
    )?;
    Ok(hash_a == hash_b)
}

pub(super) fn fetch_member_serial_tail(
    exec: &dyn HostExec,
    member: &DesiredMember,
    vm_id: &str,
    bin: &RemoteOutbeUp,
) -> String {
    let cmd = bin.cmd(&format!(
        "vm logs --name {} --expect-vm-id {} --lines {} --vmm-home {} --json",
        shell_quote(&member.vm_name),
        shell_quote(vm_id),
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
            return format!(
                "\n\n--- serial log tail for {} ({}) ---\n{}",
                member.vm_name,
                member.vmm_host.host,
                logs.log.trim_end()
            );
        }
    }
    String::new()
}

fn find_crs_file(artifacts: &Path) -> Result<PathBuf> {
    let founder_crs = artifacts.join("vm/validator-0/.bb-crs/bn254_g1.dat");
    if founder_crs.is_file() {
        return Ok(founder_crs);
    }
    bail!(
        "CRS file missing at {} (expected built VM network artifacts)",
        founder_crs.display()
    );
}

fn copy_file_mode(src: &Path, dst: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn copy_keys_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn extract_tx_hash(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .find(|word| {
            word.len() == 66
                && word.starts_with("0x")
                && word[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .ok_or_else(|| eyre::eyre!("outbe-cli did not return a transaction hash: {output}"))
}
