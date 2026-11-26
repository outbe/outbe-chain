//! Outbe VM lifecycle commands: create, host-reserve, start, stop, remove, status.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use alloy_primitives::U256;
use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    hyperlane::{ensure_setup_via_rpc, SetupState},
    progress::{Progress, WatchDisplay},
    rpc::ConsensusStatus,
    validator_address,
};
use outbe_operator_vm::{
    disk::{build_disk_upload_and_convert_command, sha256_sparse_file, stream_sparse_tar_gz},
    dnsmasq::{
        build_host_reserve_write_command, build_privileged_read_command,
        check_dnsmasq_ip_conflicts, check_dnsmasq_lease_conflicts, dnsmasq_entry,
        merge_dnsmasq_hosts, missing_dnsmasq_entries, parse_sentinel_read, sha256_hex,
        strip_dnsmasq_network_block, DEFAULT_DNSMASQ_LOCK_FILE, DNSMASQ_LEASE_DURATION,
        DNSMASQ_RESERVATIONS_FILENAME,
    },
    image::{
        build_image_publish_command, build_remote_image_check_command, decide_remote_image_action,
        validate_image_dir, RemoteImageAction,
    },
    vmm::{
        build_prpc_curl_command, build_serial_log_command, calc_app_id, extract_primary_mac,
        parse_curl_http_response, remote_vmm_path, CreateVmRequest, GetInfoResponse,
        NetworkingConfig, StatusRequest, VmInfo, VmmClient, VmmTransport, APP_COMPOSE_JSON,
    },
};
use tokio::task::JoinSet;

use crate::{
    network::{
        build::DeploymentKind,
        config::{Ssh, VmFlavor},
        remote::{
            rpc_client_via_ssh, shell_quote, DirectSshOptions as SshOptions, NetworkArtifacts,
        },
        status::rpc_ready_for_hyperlane,
    },
    vm::{
        disk,
        state::{VmFounderPhase, VmFounderState, VmState, VM_STATE_VERSION},
    },
};

const SERIAL_LOG_TAIL_LINES: usize = 40;

#[derive(Clone)]
struct SshVmmTransport {
    ssh: Ssh,
    ssh_options: SshOptions,
    vmm_home: String,
    label: String,
}

impl VmmTransport for SshVmmTransport {
    fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
        let command = build_prpc_curl_command(&self.vmm_home, method);
        let raw = self
            .ssh_options
            .capture_ssh_stdin(&self.ssh, &self.label, &command, payload)?;
        Ok(parse_curl_http_response(method, &raw)?.to_owned())
    }

    fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
        let command = build_serial_log_command(&self.vmm_home, vm_id, lines)?;
        self.ssh_options
            .capture_ssh(&self.ssh, &self.label, &command)
    }
}

fn ssh_vmm_client(
    ssh: Ssh,
    ssh_options: SshOptions,
    vmm_home: impl Into<String>,
    label: impl Into<String>,
) -> VmmClient<SshVmmTransport> {
    let label = label.into();
    let transport = SshVmmTransport {
        ssh,
        ssh_options,
        vmm_home: vmm_home.into(),
        label: label.clone(),
    };
    VmmClient::new(transport, label)
}

pub struct CreateOptions<'a> {
    pub artifacts: &'a Path,
    pub image_dir: &'a Path,
    pub image_name: Option<&'a str>,
    pub disks_dir: Option<&'a Path>,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub no_tee: bool,
}

#[derive(Clone, Debug)]
struct VmFounderStatus {
    name: String,
    vm_id: String,
    bridge_ip: Ipv4Addr,
    mac: String,
    vmm_status: String,
    vmm_running: bool,
    chain_id: Option<u64>,
    finalized: Option<u64>,
    execution_peers: Option<u64>,
    consensus: Option<ConsensusStatus>,
    hyperlane_registered: Option<bool>,
    error: Option<String>,
}

pub async fn create(options: CreateOptions<'_>) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("validating VM network artifacts and guest image");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure_vm_deployment(&build)?;

    // Resolve every founder first so we know flavors, hosts, and DHCP files.
    let mut resolved_founders = Vec::with_capacity(build.config.founders.len());
    let mut unique_hosts: BTreeMap<(String, u16, String, String), Ssh> = BTreeMap::new();
    let mut flavors = BTreeSet::new();
    for (index, founder) in build.config.founders.iter().enumerate() {
        let resolved = build.config.resolve_founder_vm(founder)?;
        flavors.insert(resolved.flavor);
        let vm_name = founder_vm_name(&build.config.name, &founder.name);
        let host_key = (
            resolved.vmm_host.host.clone(),
            resolved.vmm_host.port,
            resolved.vmm_host.user.clone(),
            resolved.vmm_home.clone(),
        );
        unique_hosts
            .entry(host_key)
            .or_insert_with(|| resolved.vmm_host.clone());
        resolved_founders.push((index, founder.name.clone(), vm_name, resolved));
    }

    let flavor_label = match flavors.len() {
        1 => match flavors.iter().next().copied().unwrap() {
            VmFlavor::Dev => "dev",
            VmFlavor::Prod => "prod",
        },
        _ => "mixed",
    };
    let (image_name, os_image_hash) =
        validate_image_dir(options.image_dir, options.image_name, flavor_label)?;

    // Load partial vm-state.json for resume, or refuse when a complete state already exists.
    let mut state =
        load_or_init_create_state(options.artifacts, &build, &image_name, &os_image_hash)?;

    let ssh_options = SshOptions::new(options.ssh_key, options.ssh_config)?;

    // Build map of already-journaled (vm_name -> vm_id) so resuming skips its own VMs.
    let resumed_ids: BTreeMap<String, String> = state
        .founders
        .iter()
        .map(|f| (f.vm_name.clone(), f.vm_id.clone()))
        .collect();
    let resumed_macs: BTreeMap<usize, String> = state
        .founders
        .iter()
        .filter(|f| !f.mac.is_empty())
        .map(|f| (f.index, f.mac.clone()))
        .collect();

    // Preflight: refuse name collisions, missing resumed VMs, and bridge IP conflicts BEFORE CreateVm.
    progress.stage("checking VMM hosts for existing VM names and bridge IP conflicts");
    for ((host, _port, _user, vmm_home), ssh) in &unique_hosts {
        let client = ssh_vmm_client(ssh.clone(), ssh_options.clone(), vmm_home.clone(), host);
        let existing = client.status(&StatusRequest {
            brief: true,
            ..StatusRequest::default()
        })?;
        let planned_names: Vec<&str> = resolved_founders
            .iter()
            .filter(|(_, _, _, resolved)| {
                resolved.vmm_host == *ssh && resolved.vmm_home == *vmm_home
            })
            .map(|(_, _, vm_name, _)| vm_name.as_str())
            .collect();
        check_no_name_collision(host, &existing.vms, &planned_names, &resumed_ids)?;

        let host_resumed: Vec<&VmFounderState> = state
            .founders
            .iter()
            .filter(|f| f.vmm_host == *ssh && f.vmm_home == *vmm_home)
            .collect();
        verify_resumed_founders_exist(host, &existing.vms, &host_resumed)?;

        // Check DHCP hosts and leases files on this host for any foreign reservation of our IPs.
        let mut dhcp_groups: BTreeMap<(PathBuf, PathBuf), Vec<(String, Ipv4Addr)>> =
            BTreeMap::new();
        for (index, _, _, resolved) in &resolved_founders {
            if resolved.vmm_host == *ssh && resolved.vmm_home == *vmm_home {
                let mac = resumed_macs.get(index).cloned().unwrap_or_default();
                dhcp_groups
                    .entry((
                        resolved.dhcp_hosts_file.clone(),
                        resolved.dhcp_leases_file.clone(),
                    ))
                    .or_default()
                    .push((mac, resolved.bridge_ip));
            }
        }
        for ((hosts_file, leases_file), requested) in dhcp_groups {
            if let Some(existing_hosts) =
                read_remote_file_privileged(&ssh_options, ssh, host, &hosts_file)?
            {
                check_dnsmasq_ip_conflicts(&existing_hosts, &requested, &build.config.name)?;
            }
            if let Some(existing_leases) =
                read_remote_file_privileged(&ssh_options, ssh, host, &leases_file)?
            {
                check_dnsmasq_lease_conflicts(&existing_leases, &requested)?;
            }
        }
    }

    // Upload guest OS image once per unique (vmm_host, vmm_home), never deleting existing images.
    progress.stage(&format!("ensuring guest image `{image_name}` on VMM hosts"));
    for ((host, _port, _user, vmm_home), ssh) in &unique_hosts {
        ensure_remote_image(
            &ssh_options,
            ssh,
            host,
            vmm_home,
            &image_name,
            &os_image_hash,
            options.image_dir,
        )?;
    }

    let app_id = calc_app_id(APP_COMPOSE_JSON);
    let temp_disks = tempfile::tempdir().wrap_err("create temporary disk staging directory")?;

    for (index, founder_name, vm_name, resolved) in resolved_founders {
        if let Some(existing) = state.founders.iter().find(|f| f.index == index) {
            if existing.phase == VmFounderPhase::Ready {
                progress.update(&format!(
                    "skipping already-ready VM `{vm_name}` ({})",
                    existing.vm_id
                ));
                continue;
            }
        }

        let client = ssh_vmm_client(
            resolved.vmm_host.clone(),
            ssh_options.clone(),
            resolved.vmm_home.clone(),
            &founder_name,
        );

        // Step 1: CreateVm and IMMEDIATELY journal vm_id before any later fallible call.
        let vm_id = match state.founders.iter().find(|f| f.index == index) {
            Some(existing) => existing.vm_id.clone(),
            None => {
                progress.stage(&format!("creating VM `{vm_name}` for {founder_name}"));
                let created_id = client.create_vm(&CreateVmRequest {
                    name: vm_name.clone(),
                    image: image_name.clone(),
                    compose_file: APP_COMPOSE_JSON.to_owned(),
                    vcpu: resolved.vcpu,
                    memory: resolved.memory_gib * 1024,
                    disk_size: resolved.disk_gib,
                    ports: Vec::new(),
                    app_id: Some(app_id.clone()),
                    user_config: String::new(),
                    hugepages: false,
                    pin_numa: false,
                    stopped: true,
                    no_tee: options.no_tee,
                    networking: Some(NetworkingConfig {
                        mode: "bridge".to_owned(),
                        bridge_name: resolved.bridge.clone(),
                    }),
                    annotations: BTreeMap::new(),
                })?;

                let entry = VmFounderState {
                    index,
                    name: founder_name.clone(),
                    vm_name: vm_name.clone(),
                    vm_id: created_id.clone(),
                    vmm_host: resolved.vmm_host.clone(),
                    vmm_home: resolved.vmm_home.clone(),
                    bridge: resolved.bridge.clone(),
                    bridge_ip: resolved.bridge_ip,
                    mac: String::new(),
                    dhcp_hosts_file: resolved.dhcp_hosts_file.clone(),
                    dhcp_leases_file: resolved.dhcp_leases_file.clone(),
                    dhcp_pid_file: resolved.dhcp_pid_file.clone(),
                    phase: VmFounderPhase::Created,
                };
                journal_created_vm(options.artifacts, &mut state, entry, |id| {
                    client.remove_vm(id)
                })?;
                created_id
            }
        };

        // Step 2: Fetch MAC via GetInfo if not yet recorded, and stage qcow2 disk.
        let current_phase = state
            .founders
            .iter()
            .find(|f| f.index == index)
            .map(|f| f.phase)
            .unwrap_or(VmFounderPhase::Created);

        if current_phase == VmFounderPhase::Created {
            let info_resp = client.get_info(&vm_id)?;
            ensure!(
                info_resp.found,
                "VMM GetInfo reported VM {vm_id} not found immediately after CreateVm"
            );
            let info = info_resp.info.ok_or_else(|| {
                eyre::eyre!("VMM GetInfo did not return VM info for newly created {vm_id}")
            })?;
            let mac = extract_primary_mac(&info)?;

            let raw_disk = match options.disks_dir {
                Some(dir) => {
                    let candidate = dir.join(format!("validator-{index}.img"));
                    ensure!(
                        candidate.is_file(),
                        "pre-built founder disk does not exist: {}",
                        candidate.display()
                    );
                    candidate
                }
                None => {
                    let staged = temp_disks.path().join(format!("validator-{index}.img"));
                    disk::run(
                        options.artifacts,
                        &index.to_string(),
                        &staged,
                        Some(resolved.disk_gib),
                    )?;
                    staged
                }
            };

            progress.stage(&format!(
                "uploading and converting data disk for {founder_name} ({vm_id})"
            ));
            upload_and_convert_disk(
                &ssh_options,
                &resolved.vmm_host,
                &founder_name,
                &resolved.vmm_home,
                &vm_id,
                &raw_disk,
                resolved.disk_gib,
            )?;

            let entry = state
                .founders
                .iter_mut()
                .find(|f| f.index == index)
                .expect("journaled founder entry must exist");
            entry.mac = mac;
            entry.phase = VmFounderPhase::DiskStaged;
            state.save(options.artifacts)?;
        }

        let entry = state
            .founders
            .iter_mut()
            .find(|f| f.index == index)
            .expect("journaled founder entry must exist");
        entry.phase = VmFounderPhase::Ready;
        state.save(options.artifacts)?;
    }

    let state_path = state.save(options.artifacts)?;

    // Write dnsmasq reservation entries locally for operator review without mutating hosts.
    let reservations_path = options.artifacts.join(DNSMASQ_RESERVATIONS_FILENAME);
    let reservations_body = state
        .founders
        .iter()
        .map(|founder| dnsmasq_entry(&founder.mac, founder.bridge_ip))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&reservations_path, format!("{reservations_body}\n"))?;

    progress.done(&format!(
        "created {} stopped VMs and wrote {}",
        state.founders.len(),
        state_path.display()
    ));

    // Verify dnsmasq reservations read-only on each VMM host and report missing entries.
    report_dnsmasq_status(&state, &ssh_options, options.artifacts)?;
    Ok(())
}

pub fn host_reserve(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    create: bool,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM deployment state");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let state = VmState::load(artifacts)?;
    state.ensure_complete_for_artifacts(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    let grouped = group_dnsmasq_entries(&state);

    for ((host, _port, _user, hosts_file, leases_file, pid_file), (ssh, entries)) in grouped {
        progress.stage(&format!("updating {} on {host}", hosts_file.display()));

        // Fail closed on any read/permission/SSH error. Missing file requires `--create`.
        let existing_opt = read_remote_file_privileged(&ssh_options, &ssh, &host, &hosts_file)?;
        let (existing_hosts, expected_sha256) = match existing_opt {
            Some(content) => {
                let sha = sha256_hex(content.as_bytes());
                (content, Some(sha))
            }
            None => {
                ensure!(
                    create,
                    "dnsmasq hosts file {} does not exist on {host}; pass --create to create it",
                    hosts_file.display()
                );
                (String::new(), None)
            }
        };

        // Check active leases for IP conflicts before writing.
        if let Some(existing_leases) =
            read_remote_file_privileged(&ssh_options, &ssh, &host, &leases_file)?
        {
            check_dnsmasq_lease_conflicts(&existing_leases, &entries)?;
        }

        // Merge idempotently while refusing any foreign IP reservation conflict.
        let merged = merge_dnsmasq_hosts(&existing_hosts, &entries, &state.network_name)?;

        // Acquire root-owned flock, verify CAS digest, write to unique mktemp file, atomic mv, and SIGHUP.
        let write_cmd = build_host_reserve_write_command(
            &hosts_file,
            &pid_file,
            Path::new(DEFAULT_DNSMASQ_LOCK_FILE),
            expected_sha256.as_deref(),
        );
        ssh_options
            .capture_ssh_stdin(&ssh, &host, &write_cmd, merged.as_bytes())
            .wrap_err_with(|| format!("update {} on {host}", hosts_file.display()))?;
    }

    progress.done(&format!(
        "applied DHCP reservations for {} founders and reloaded dnsmasq",
        state.founders.len()
    ));
    Ok(())
}

/// Remove only this network's managed dnsmasq reservation block and reload dnsmasq.
///
/// Works from `network.yaml` / `network-manifest.json` alone so it can run after `vm remove`
/// has cleared `vm-state.json`.
pub fn host_release(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM network artifacts");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    let mut targets: BTreeMap<(String, u16, String, PathBuf, PathBuf), Ssh> = BTreeMap::new();
    for founder in &build.config.founders {
        let resolved = build.config.resolve_founder_vm(founder)?;
        let key = (
            resolved.vmm_host.host.clone(),
            resolved.vmm_host.port,
            resolved.vmm_host.user.clone(),
            resolved.dhcp_hosts_file.clone(),
            resolved.dhcp_pid_file.clone(),
        );
        targets.entry(key).or_insert(resolved.vmm_host);
    }

    let mut updated_hosts = 0usize;
    for ((host, _port, _user, hosts_file, pid_file), ssh) in targets {
        progress.stage(&format!(
            "releasing {} reservations in {} on {host}",
            build.config.name,
            hosts_file.display()
        ));
        let Some(existing) = read_remote_file_privileged(&ssh_options, &ssh, &host, &hosts_file)?
        else {
            continue;
        };
        let stripped = strip_dnsmasq_network_block(&existing, &build.config.name);
        if stripped == existing {
            continue;
        }
        let expected_sha256 = sha256_hex(existing.as_bytes());
        let write_cmd = build_host_reserve_write_command(
            &hosts_file,
            &pid_file,
            Path::new(DEFAULT_DNSMASQ_LOCK_FILE),
            Some(&expected_sha256),
        );
        ssh_options
            .capture_ssh_stdin(&ssh, &host, &write_cmd, stripped.as_bytes())
            .wrap_err_with(|| {
                format!("release reservations in {} on {host}", hosts_file.display())
            })?;
        updated_hosts += 1;
    }

    progress.done(&format!(
        "released DHCP reservations for network `{}` on {updated_hosts} host(s)",
        build.config.name
    ));
    Ok(())
}

pub async fn start(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    timeout: Duration,
    max_lag: u64,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM deployment state");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let state = VmState::load(artifacts)?;
    state.ensure_complete_for_artifacts(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    progress.stage(&format!(
        "starting {} VMs concurrently",
        state.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in state.founders.clone() {
        let ssh_options = ssh_options.clone();
        tasks.spawn_blocking(move || {
            let client = ssh_vmm_client(
                founder.vmm_host,
                ssh_options,
                founder.vmm_home,
                &founder.name,
            );
            client
                .start_vm(&founder.vm_id)
                .wrap_err_with(|| format!("start VM {} ({})", founder.name, founder.vm_id))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("VM start task panicked")??;
    }

    let required_peers = state.founders.len().saturating_sub(1);
    let std_deadline = Instant::now() + timeout;
    let tokio_deadline = tokio::time::Instant::from_std(std_deadline);
    let mut hyperlane_configured = false;
    let mut last_statuses: Vec<VmFounderStatus> = Vec::new();
    progress.stage("waiting for VMs to reach finalized consensus");

    loop {
        let Some(statuses) =
            run_with_deadline(tokio_deadline, query_all(&build, &state, &ssh_options)).await?
        else {
            bail!(
                "{}",
                build_timeout_report(
                    &build,
                    &state,
                    &ssh_options,
                    timeout,
                    &last_statuses,
                    max_lag,
                    "finalized consensus"
                )
            );
        };
        last_statuses = statuses.clone();

        if !hyperlane_configured
            && statuses
                .iter()
                .all(|status| vm_ready_for_hyperlane(status, build.config.chain_id, required_peers))
        {
            progress.stage("configuring Hyperlane signers on chain via SSH RPC transport");
            // Cooperative deadline: never cancel an in-flight transaction broadcast.
            let completed =
                configure_hyperlane_vms(&build, &state, &ssh_options, std_deadline, &mut progress)
                    .await?;
            if !completed {
                bail!(
                    "{}",
                    build_timeout_report(
                        &build,
                        &state,
                        &ssh_options,
                        timeout,
                        &last_statuses,
                        max_lag,
                        "Hyperlane setup"
                    )
                );
            }
            hyperlane_configured = true;
            progress.stage("verifying Hyperlane signer registrations");
            continue;
        }
        let assessments = assess_founders_lag(&statuses, max_lag);
        if hyperlane_configured
            && statuses.iter().all(|status| {
                vm_ready_for_hyperlane(status, build.config.chain_id, required_peers)
                    && status.hyperlane_registered == Some(true)
            })
            && assessments.iter().all(|assessment| !assessment.lagging)
        {
            progress.done(&format!(
                "VM network {} finalized with {} founding validators",
                build.config.name,
                statuses.len()
            ));
            println!("\n{}", render_status(&build, &statuses, max_lag));
            return Ok(());
        }
        if Instant::now() >= std_deadline {
            bail!(
                "{}",
                build_timeout_report(
                    &build,
                    &state,
                    &ssh_options,
                    timeout,
                    &statuses,
                    max_lag,
                    if hyperlane_configured {
                        "Hyperlane signer verification and consensus catchup"
                    } else {
                        "finalized consensus"
                    }
                )
            );
        }
        let ready = statuses
            .iter()
            .zip(&assessments)
            .filter(|(status, assessment)| {
                if hyperlane_configured {
                    vm_ready_for_hyperlane(status, build.config.chain_id, required_peers)
                        && status.hyperlane_registered == Some(true)
                        && !assessment.lagging
                } else {
                    vm_ready_for_hyperlane(status, build.config.chain_id, required_peers)
                }
            })
            .count();
        progress.update(&format!("{ready} of {} ready", statuses.len()));

        let remaining = std_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            continue;
        }
        tokio::time::sleep(remaining.min(Duration::from_secs(5))).await;
    }
}

pub async fn stop(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM deployment state");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let state = VmState::load(artifacts)?;
    state.ensure_matches_network(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    progress.stage(&format!("stopping {} recorded VMs", state.founders.len()));
    let mut tasks = JoinSet::new();
    for founder in state.founders.clone() {
        let ssh_options = ssh_options.clone();
        tasks.spawn_blocking(move || {
            let client = ssh_vmm_client(
                founder.vmm_host,
                ssh_options,
                founder.vmm_home,
                &founder.name,
            );
            client
                .stop_vm(&founder.vm_id)
                .wrap_err_with(|| format!("stop VM {} ({})", founder.name, founder.vm_id))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("VM stop task panicked")??;
    }
    progress.done(&format!("stopped {} VMs", state.founders.len()));
    Ok(())
}

pub async fn remove(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    release_reservations: bool,
    timeout: Duration,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("loading VM deployment state");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let state = VmState::load(artifacts)?;
    // Allow removing partial states left by an interrupted `vm create`.
    state.ensure_matches_network(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    progress.stage(&format!(
        "stopping and removing {} recorded VMs (leaving guest images intact)",
        state.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in state.founders.clone() {
        let ssh_options = ssh_options.clone();
        tasks.spawn_blocking(move || {
            let client = ssh_vmm_client(
                founder.vmm_host,
                ssh_options,
                founder.vmm_home,
                &founder.name,
            );
            // Ignore StopVm errors when the VM is already stopped.
            let _ = client.stop_vm(&founder.vm_id);
            client
                .remove_vm(&founder.vm_id)
                .wrap_err_with(|| format!("remove VM {} ({})", founder.name, founder.vm_id))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("VM remove task panicked")??;
    }

    // RemoveVm is asynchronous in dstack-vmm; poll GetInfo until every VM is gone before
    // deleting vm-state.json so an operator can safely re-run `vm create` or retry `vm remove`.
    progress.stage("waiting for asynchronous VMM removal to complete");
    let deadline = Instant::now() + timeout;
    loop {
        let mut remaining_vms = Vec::new();
        for founder in &state.founders {
            let client = ssh_vmm_client(
                founder.vmm_host.clone(),
                ssh_options.clone(),
                founder.vmm_home.clone(),
                &founder.name,
            );
            let vm_id = founder.vm_id.clone();
            let info = tokio::task::spawn_blocking(move || client.get_info(&vm_id))
                .await
                .wrap_err("VM remove poll task panicked")??;
            if !vm_is_gone(&info) {
                let status = info
                    .info
                    .map(|i| i.status)
                    .unwrap_or_else(|| "unknown".to_owned());
                remaining_vms.push(format!("{} ({}: {status})", founder.name, founder.vm_id));
            }
        }
        if remaining_vms.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "VMs did not finish removing within {} seconds (keeping vm-state.json for retry): {}",
                timeout.as_secs(),
                remaining_vms.join(", ")
            );
        }
        progress.update(&format!(
            "waiting for {} VM(s) to finish removing",
            remaining_vms.len()
        ));
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            continue;
        }
        tokio::time::sleep(left.min(Duration::from_secs(2))).await;
    }

    let state_path = VmState::path(artifacts);
    if state_path.exists() {
        fs::remove_file(&state_path)
            .wrap_err_with(|| format!("remove {}", state_path.display()))?;
    }
    progress.done(&format!(
        "removed {} VMs and cleared {}",
        state.founders.len(),
        state_path.display()
    ));

    if release_reservations {
        host_release(artifacts, ssh_key, ssh_config)?;
    } else {
        println!(
            "Note: managed dnsmasq reservations (`# outbe-up:{}:begin..end`) are left in place \
             (leases expire in {}). To release them now:\n  outbe-up vm host-release --artifacts {}",
            state.network_name,
            DNSMASQ_LEASE_DURATION,
            artifacts.display()
        );
    }
    Ok(())
}

pub async fn status(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    watch: bool,
    max_lag: u64,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("querying VM founder status");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure_vm_deployment(&build)?;
    let state = VmState::load(artifacts)?;
    state.ensure_complete_for_artifacts(&build)?;
    let ssh_options = SshOptions::new(ssh_key, ssh_config)?;

    let statuses = query_all(&build, &state, &ssh_options).await?;
    if !watch {
        progress.done(&format!("queried {} VM founders", statuses.len()));
        println!("\n{}", render_status(&build, &statuses, max_lag));
        let assessments = assess_founders_lag(&statuses, max_lag);
        let unhealthy: Vec<String> = statuses
            .iter()
            .zip(&assessments)
            .filter_map(|(status, assessment)| {
                if !status.vmm_running || status.error.is_some() || status.finalized.is_none() {
                    Some(format!("{}: unreachable/not-running", status.name))
                } else if assessment.lagging {
                    Some(format!(
                        "{}: {}",
                        status.name,
                        assessment.reasons.join(", ")
                    ))
                } else {
                    None
                }
            })
            .collect();
        if !unhealthy.is_empty() {
            bail!(
                "VM network has unreachable or lagging founder(s): {}",
                unhealthy.join("; ")
            );
        }
        return Ok(());
    }

    progress.done(&format!("connected to {} VM founders", statuses.len()));
    let display = WatchDisplay::new();
    display.render(&render_status(&build, &statuses, max_lag));
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
        display.render(&render_status(
            &build,
            &query_all(&build, &state, &ssh_options).await?,
            max_lag,
        ));
    }
}

fn vm_is_gone(resp: &GetInfoResponse) -> bool {
    !resp.found || resp.info.is_none()
}

/// Run an async operation bounded by `deadline`, returning `Ok(None)` if the deadline expires.
async fn run_with_deadline<F, T>(deadline: tokio::time::Instant, future: F) -> Result<Option<T>>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout_at(deadline, future).await {
        Ok(result) => result.map(Some),
        Err(_) => Ok(None),
    }
}

fn build_timeout_report(
    build: &NetworkArtifacts,
    state: &VmState,
    ssh_options: &SshOptions,
    timeout: Duration,
    statuses: &[VmFounderStatus],
    max_lag: u64,
    stage: &str,
) -> String {
    let mut report = format!(
        "VM network did not finish {stage} within {} seconds:\n{}\n",
        timeout.as_secs(),
        render_status(build, statuses, max_lag)
    );
    for founder in &state.founders {
        let client = ssh_vmm_client(
            founder.vmm_host.clone(),
            ssh_options.clone(),
            founder.vmm_home.clone(),
            &founder.name,
        );
        let tail = client
            .serial_log_tail(&founder.vm_id, SERIAL_LOG_TAIL_LINES)
            .unwrap_or_else(|err| format!("(failed to read serial log: {err})"));
        report.push_str(&format!(
            "\n--- {} ({}) serial.log tail ---\n{}\n",
            founder.name, founder.vm_id, tail
        ));
    }
    report
}

async fn query_all(
    build: &NetworkArtifacts,
    state: &VmState,
    ssh_options: &SshOptions,
) -> Result<Vec<VmFounderStatus>> {
    let mut tasks = JoinSet::new();
    for founder in state.founders.clone() {
        let ssh_options = ssh_options.clone();
        let validator_key = build
            .root
            .join(format!("vm/validator-{}/keys/evm-key.hex", founder.index));
        let hyperlane_key = build.root.join(format!(
            "vm/validator-{}/keys/hyperlane-key.hex",
            founder.index
        ));
        tasks.spawn(
            async move { query_one(founder, ssh_options, validator_key, hyperlane_key).await },
        );
    }
    let mut statuses = Vec::with_capacity(state.founders.len());
    while let Some(result) = tasks.join_next().await {
        statuses.push(result.wrap_err("VM status task panicked")?);
    }
    statuses.sort_by_key(|status| {
        state
            .founders
            .iter()
            .position(|founder| founder.name == status.name)
            .unwrap_or(usize::MAX)
    });
    Ok(statuses)
}

async fn query_one(
    founder: VmFounderState,
    ssh_options: SshOptions,
    validator_key: PathBuf,
    hyperlane_key: PathBuf,
) -> VmFounderStatus {
    let vmm_client = ssh_vmm_client(
        founder.vmm_host.clone(),
        ssh_options.clone(),
        founder.vmm_home.clone(),
        &founder.name,
    );
    let vm_id = founder.vm_id.clone();
    let vmm_info = tokio::task::spawn_blocking(move || vmm_client.get_info(&vm_id)).await;

    let (vmm_status, vmm_running, mut error) = match vmm_info {
        Ok(Ok(resp)) if resp.found => match resp.info {
            Some(info) => {
                let boot_err = (!info.boot_error.is_empty()).then_some(info.boot_error);
                (info.status, info.running, boot_err)
            }
            None => (
                "missing".to_owned(),
                false,
                Some("VM not found on VMM host".to_owned()),
            ),
        },
        Ok(Ok(_)) => (
            "missing".to_owned(),
            false,
            Some("VM not found on VMM host".to_owned()),
        ),
        Ok(Err(err)) => ("unreachable".to_owned(), false, Some(format!("{err:#}"))),
        Err(err) => ("error".to_owned(), false, Some(format!("{err:#}"))),
    };

    if !vmm_running {
        return VmFounderStatus {
            name: founder.name,
            vm_id: founder.vm_id,
            bridge_ip: founder.bridge_ip,
            mac: founder.mac,
            vmm_status,
            vmm_running,
            chain_id: None,
            finalized: None,
            execution_peers: None,
            consensus: None,
            hyperlane_registered: None,
            error,
        };
    }

    let rpc = rpc_client_via_ssh(
        format!("http://{}:8545", founder.bridge_ip),
        founder.vmm_host.clone(),
        ssh_options,
        &founder.name,
    );
    let chain_id = rpc.chain_id().await.ok();
    let finalized = rpc.finalized_block().await.ok().map(|b| b.number);
    let execution_peers = rpc.peer_count().await.ok();
    let consensus = rpc.consensus_status().await.ok();
    let hyperlane_registered = match (
        validator_address(&validator_key).ok(),
        validator_address(&hyperlane_key).ok(),
    ) {
        (Some(val_addr), Some(hl_addr)) => rpc
            .hyperlane_signer(val_addr)
            .await
            .ok()
            .map(|registered| registered == hl_addr),
        _ => None,
    };
    if chain_id.is_none() && error.is_none() {
        error = Some("RPC unreachable on bridge IP".to_owned());
    }

    VmFounderStatus {
        name: founder.name,
        vm_id: founder.vm_id,
        bridge_ip: founder.bridge_ip,
        mac: founder.mac,
        vmm_status,
        vmm_running,
        chain_id,
        finalized,
        execution_peers,
        consensus,
        hyperlane_registered,
        error,
    }
}

fn vm_ready_for_hyperlane(status: &VmFounderStatus, chain_id: u64, required_peers: usize) -> bool {
    status.vmm_running
        && status.error.is_none()
        && rpc_ready_for_hyperlane(
            status.chain_id,
            status.finalized,
            status.execution_peers,
            status.consensus.as_ref(),
            chain_id,
            required_peers,
        )
}

async fn configure_hyperlane_vms(
    build: &NetworkArtifacts,
    state: &VmState,
    ssh_options: &SshOptions,
    deadline: Instant,
    progress: &mut Progress,
) -> Result<bool> {
    let funding = build
        .config
        .hyperlane
        .signer_funding_wei
        .parse::<U256>()
        .wrap_err("hyperlane.signer_funding_wei must be an integer")?;

    for founder in &state.founders {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let validator_key = build
            .root
            .join(format!("vm/validator-{}/keys/evm-key.hex", founder.index));
        let hyperlane_key = build.root.join(format!(
            "vm/validator-{}/keys/hyperlane-key.hex",
            founder.index
        ));
        let setup_state_path = build
            .root
            .join(format!("hyperlane-setup-{}.json", founder.index));
        let setup_state: SetupState = if setup_state_path.is_file() {
            serde_json::from_slice(&fs::read(&setup_state_path)?)?
        } else {
            SetupState::default()
        };
        let rpc = rpc_client_via_ssh(
            format!("http://{}:8545", founder.bridge_ip),
            founder.vmm_host.clone(),
            ssh_options.clone(),
            &founder.name,
        );
        progress.stage(&format!(
            "configuring Hyperlane signer for {}",
            founder.name
        ));
        let (updated, completed) = ensure_setup_via_rpc(
            &rpc,
            &validator_key,
            &hyperlane_key,
            funding,
            setup_state,
            Some(deadline),
            progress,
            |next| {
                fs::write(&setup_state_path, serde_json::to_vec_pretty(next)?)?;
                Ok(())
            },
        )
        .await
        .wrap_err_with(|| format!("configure Hyperlane for {}", founder.name))?;
        fs::write(&setup_state_path, serde_json::to_vec_pretty(&updated)?)?;
        if !completed {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FounderLagAssessment {
    lag: Option<u64>,
    lagging: bool,
    reasons: Vec<String>,
}

/// Compare each founder's finalized height and `vrfMaterialVersion` against the cohort.
fn assess_founders_lag(statuses: &[VmFounderStatus], max_lag: u64) -> Vec<FounderLagAssessment> {
    let highest_finalized = statuses.iter().filter_map(|s| s.finalized).max();

    // Determine majority `vrfMaterialVersion` across founders reporting consensus status.
    let mut vrf_counts: BTreeMap<u64, usize> = BTreeMap::new();
    let mut vrf_total = 0usize;
    for status in statuses {
        if let Some(c) = &status.consensus {
            *vrf_counts.entry(c.vrf_material_version).or_insert(0) += 1;
            vrf_total += 1;
        }
    }
    let majority_vrf = vrf_counts
        .into_iter()
        .find(|(_, count)| *count * 2 > vrf_total)
        .map(|(version, _)| version);

    statuses
        .iter()
        .map(|status| {
            let lag = match (highest_finalized, status.finalized) {
                (Some(max_h), Some(h)) => Some(max_h.saturating_sub(h)),
                _ => None,
            };
            let mut reasons = Vec::new();
            if let Some(delta) = lag {
                if delta > max_lag {
                    reasons.push(format!("lag {delta} > {max_lag}"));
                }
            }
            if let (Some(majority), Some(c)) = (majority_vrf, &status.consensus) {
                if c.vrf_material_version != majority {
                    reasons.push(format!(
                        "vrf {} != majority {majority}",
                        c.vrf_material_version
                    ));
                }
            }
            let lagging = !reasons.is_empty();
            FounderLagAssessment {
                lag,
                lagging,
                reasons,
            }
        })
        .collect()
}

fn render_status(build: &NetworkArtifacts, statuses: &[VmFounderStatus], max_lag: u64) -> String {
    let required_peers = build.config.founders.len().saturating_sub(1);
    let assessments = assess_founders_lag(statuses, max_lag);
    let ready_count = statuses
        .iter()
        .zip(&assessments)
        .filter(|(status, assessment)| {
            vm_ready_for_hyperlane(status, build.config.chain_id, required_peers)
                && status.hyperlane_registered == Some(true)
                && !assessment.lagging
        })
        .count();
    let mut output = format!(
        "VM Network  {} | chain {} | ready {ready_count}/{}\n\n",
        build.config.name,
        build.config.chain_id,
        statuses.len()
    );
    output.push_str(
        "Founder              VMM       Bridge IP        MAC                Finalized (Lag)  Exec  HL   Consensus\n",
    );
    for (status, assessment) in statuses.iter().zip(&assessments) {
        let finalized = match (status.finalized, assessment.lag) {
            (Some(n), Some(0)) => format!("{n} (0)"),
            (Some(n), Some(lag)) => format!("{n} (-{lag})"),
            (Some(n), None) => n.to_string(),
            (None, _) => "?".to_owned(),
        };
        let exec = status
            .execution_peers
            .map_or_else(|| "?".to_owned(), |n| n.to_string());
        let hl = match status.hyperlane_registered {
            Some(true) => "ok",
            Some(false) => "wait",
            None => "?",
        };
        let mut consensus = status.consensus.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |c| {
                format!(
                    "{} peers={} shares={} vrf={} tee={}",
                    c.mode,
                    c.connected_peers,
                    if c.has_threshold_shares { "yes" } else { "no" },
                    c.vrf_material_version,
                    if c.resident_ready() {
                        "ready"
                    } else {
                        "not-ready"
                    },
                )
            },
        );
        if assessment.lagging {
            consensus.push_str(&format!(" [LAGGING: {}]", assessment.reasons.join(", ")));
        }
        output.push_str(&format!(
            "{:<20} {:<9} {:<16} {:<18} {:<16} {:<5} {:<4} {}\n",
            status.name,
            status.vmm_status,
            status.bridge_ip,
            status.mac,
            finalized,
            exec,
            hl,
            consensus
        ));
        if let Some(error) = &status.error {
            output.push_str(&format!("  error ({}): {error}\n", status.vm_id));
        }
    }
    output.trim_end().to_owned()
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

fn load_or_init_create_state(
    artifacts: &Path,
    build: &NetworkArtifacts,
    image_name: &str,
    os_image_hash: &str,
) -> Result<VmState> {
    let path = VmState::path(artifacts);
    if !path.exists() {
        return Ok(VmState {
            version: VM_STATE_VERSION,
            network_name: build.config.name.clone(),
            image_name: image_name.to_owned(),
            os_image_hash: os_image_hash.to_owned(),
            founders: Vec::new(),
        });
    }
    let existing = VmState::load(artifacts)?;
    existing.ensure_matches_network(build)?;
    ensure!(
        !existing.is_complete(build.config.founders.len()),
        "VM state already exists at {}; run `outbe-up vm remove --artifacts {}` before creating again",
        path.display(),
        artifacts.display()
    );
    ensure!(
        existing.image_name == image_name && existing.os_image_hash == os_image_hash,
        "partial vm-state.json uses image `{}` ({}), which differs from `{image_name}` ({os_image_hash}); run `outbe-up vm remove` first",
        existing.image_name,
        existing.os_image_hash
    );
    Ok(existing)
}

fn founder_vm_name(network_name: &str, founder_name: &str) -> String {
    format!("{network_name}-{founder_name}")
}

fn check_no_name_collision(
    host: &str,
    existing: &[VmInfo],
    planned_names: &[&str],
    resumed_ids: &BTreeMap<String, String>,
) -> Result<()> {
    let existing_by_name: BTreeMap<&str, &str> = existing
        .iter()
        .map(|vm| (vm.name.as_str(), vm.id.as_str()))
        .collect();
    for planned in planned_names {
        if let Some(&existing_id) = existing_by_name.get(planned) {
            let is_resumed = resumed_ids
                .get(*planned)
                .is_some_and(|recorded_id| recorded_id == existing_id);
            if !is_resumed {
                bail!(
                    "refusing to create VM `{planned}`: a VM with the same name ({existing_id}) already exists on VMM host {host}"
                );
            }
        }
    }
    Ok(())
}

/// Verify on resume that every recorded founder VM still exists on `host` with its expected name.
fn verify_resumed_founders_exist(
    host: &str,
    existing: &[VmInfo],
    resumed: &[&VmFounderState],
) -> Result<()> {
    let existing_by_id: BTreeMap<&str, &str> = existing
        .iter()
        .map(|vm| (vm.id.as_str(), vm.name.as_str()))
        .collect();
    for founder in resumed {
        match existing_by_id.get(founder.vm_id.as_str()) {
            Some(&actual_name) if actual_name == founder.vm_name => {}
            Some(&actual_name) => {
                bail!(
                    "recorded VM {} ({}) on {host} now has unexpected name `{actual_name}`; run `outbe-up vm remove` to clean up",
                    founder.vm_id,
                    founder.vm_name
                );
            }
            None => {
                bail!(
                    "recorded VM {} ({}) is missing from VMM host {host}; run `outbe-up vm remove` to clean up",
                    founder.vm_id,
                    founder.vm_name
                );
            }
        }
    }
    Ok(())
}

/// Persist a newly created VM into `vm-state.json`; if saving fails, remove that exact VM best-effort.
fn journal_created_vm(
    artifacts: &Path,
    state: &mut VmState,
    entry: VmFounderState,
    remove_vm: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    let created_id = entry.vm_id.clone();
    state.founders.push(entry);
    state.founders.sort_by_key(|f| f.index);
    if let Err(save_err) = state.save(artifacts) {
        state.founders.retain(|f| f.vm_id != created_id);
        let cleanup_note = match remove_vm(&created_id) {
            Ok(()) => format!("removed newly created VM {created_id}"),
            Err(rm_err) => format!(
                "failed to remove newly created VM {created_id} ({rm_err:#}); remove it manually"
            ),
        };
        bail!(
            "failed to journal newly created VM {created_id} to vm-state.json ({save_err:#}); {cleanup_note}"
        );
    }
    Ok(())
}

fn read_remote_file_privileged(
    ssh_options: &SshOptions,
    ssh: &Ssh,
    host: &str,
    path: &Path,
) -> Result<Option<String>> {
    let cmd = build_privileged_read_command(path);
    // Preserve exact bytes (including trailing newlines) so local SHA-256 matches remote sha256sum.
    let raw = ssh_options
        .capture_ssh_raw(ssh, host, &cmd)
        .wrap_err_with(|| format!("read {} on {host}", path.display()))?;
    parse_sentinel_read(&raw, path)
}

fn ensure_remote_image(
    ssh_options: &SshOptions,
    ssh: &Ssh,
    host: &str,
    vmm_home: &str,
    image_name: &str,
    local_digest: &str,
    local_image_dir: &Path,
) -> Result<()> {
    let check_cmd = build_remote_image_check_command(vmm_home, image_name);
    let raw = ssh_options
        .capture_ssh_raw(ssh, host, &check_cmd)
        .wrap_err_with(|| format!("check remote image `{image_name}` on {host}"))?;
    let remote_digest = parse_sentinel_read(&raw, Path::new(image_name))?;

    if decide_remote_image_action(remote_digest.as_deref(), local_digest, image_name)?
        == RemoteImageAction::Reuse
    {
        return Ok(());
    }

    // Stage upload into a unique temp directory and move atomically under lock without deleting existing images.
    let remote_parent_expr = remote_vmm_path(vmm_home, "image");
    let stage_template = remote_vmm_path(vmm_home, &format!("image/.{image_name}.tmp.XXXXXX"));
    let stage_dir = ssh_options
        .capture_ssh(
            ssh,
            host,
            &format!("mkdir -p {remote_parent_expr} && mktemp -d {stage_template}"),
        )
        .wrap_err_with(|| format!("create remote staging dir for `{image_name}` on {host}"))?;
    ensure!(
        !stage_dir.is_empty(),
        "remote mktemp -d returned an empty path"
    );

    let extract_cmd = format!("tar --sparse -xzf - -C {}", shell_quote(&stage_dir));
    if let Err(err) = ssh_options.stream_stdin_to_ssh(ssh, host, &extract_cmd, |sink| {
        stream_sparse_tar_gz(local_image_dir, ".", sink)
    }) {
        let _ =
            ssh_options.run_quiet_ssh(ssh, host, &format!("rm -rf {}", shell_quote(&stage_dir)));
        return Err(err);
    }

    let publish_cmd = build_image_publish_command(vmm_home, image_name, &stage_dir, local_digest);
    ssh_options.run_quiet_ssh(ssh, host, &publish_cmd)?;
    Ok(())
}

fn upload_and_convert_disk(
    ssh_options: &SshOptions,
    ssh: &Ssh,
    label: &str,
    vmm_home: &str,
    vm_id: &str,
    local_raw_disk: &Path,
    disk_gib: u32,
) -> Result<()> {
    let parent = local_raw_disk.parent().unwrap_or_else(|| Path::new("."));
    let filename = local_raw_disk
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            eyre::eyre!(
                "disk path has no UTF-8 filename: {}",
                local_raw_disk.display()
            )
        })?;
    let expected_sha256 = sha256_sparse_file(local_raw_disk)?;
    let cmd = build_disk_upload_and_convert_command(
        vmm_home,
        vm_id,
        filename,
        &expected_sha256,
        disk_gib,
    );
    ssh_options.stream_stdin_to_ssh(ssh, label, &cmd, |sink| {
        stream_sparse_tar_gz(parent, filename, sink)
    })?;
    Ok(())
}

type DnsmasqGroupKey = (String, u16, String, PathBuf, PathBuf, PathBuf);
type DnsmasqGroupValue = (Ssh, Vec<(String, Ipv4Addr)>);

fn group_dnsmasq_entries(state: &VmState) -> BTreeMap<DnsmasqGroupKey, DnsmasqGroupValue> {
    let mut grouped: BTreeMap<DnsmasqGroupKey, DnsmasqGroupValue> = BTreeMap::new();
    for founder in &state.founders {
        let key = (
            founder.vmm_host.host.clone(),
            founder.vmm_host.port,
            founder.vmm_host.user.clone(),
            founder.dhcp_hosts_file.clone(),
            founder.dhcp_leases_file.clone(),
            founder.dhcp_pid_file.clone(),
        );
        grouped
            .entry(key)
            .or_insert_with(|| (founder.vmm_host.clone(), Vec::new()))
            .1
            .push((founder.mac.clone(), founder.bridge_ip));
    }
    grouped
}

fn report_dnsmasq_status(
    state: &VmState,
    ssh_options: &SshOptions,
    artifacts: &Path,
) -> Result<()> {
    let grouped = group_dnsmasq_entries(state);
    let mut any_missing = false;
    println!("\nRequired dnsmasq reservations:");
    for ((host, _port, _user, hosts_file, _leases_file, _pid_file), (ssh, entries)) in grouped {
        let existing =
            read_remote_file_privileged(ssh_options, &ssh, &host, &hosts_file)?.unwrap_or_default();
        let missing = missing_dnsmasq_entries(&existing, &entries);
        println!("  Host {host} ({}):", hosts_file.display());
        for (mac, ip) in &entries {
            let status = if missing.iter().any(|(m, i)| m == mac && i == ip) {
                any_missing = true;
                "missing"
            } else {
                "present"
            };
            println!("    {}  [{status}]", dnsmasq_entry(mac, *ip));
        }
    }
    if any_missing {
        println!(
            "\nNext: outbe-up vm host-reserve --artifacts {}",
            artifacts.display()
        );
    } else {
        println!(
            "\nAll DHCP reservations are present. Next: outbe-up vm start --artifacts {}",
            artifacts.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID_0: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_1: &str = "550e8400-e29b-41d4-a716-446655440001";
    const UUID_FOREIGN: &str = "550e8400-e29b-41d4-a716-446655440099";

    #[test]
    fn verify_resumed_founders_rejects_missing_or_renamed_vm() {
        let recorded = VmFounderState {
            index: 0,
            name: "validator-0".into(),
            vm_name: "vm-devnet-validator-0".into(),
            vm_id: UUID_0.into(),
            vmm_host: Ssh {
                host: "192.0.2.50".into(),
                user: "ubuntu".into(),
                port: 22,
            },
            vmm_home: "~/.dstack-vmm".into(),
            bridge: "obr0".into(),
            bridge_ip: Ipv4Addr::new(10, 77, 0, 10),
            mac: String::new(),
            dhcp_hosts_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.hosts"),
            dhcp_leases_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.leases"),
            dhcp_pid_file: PathBuf::from("/run/outbe-mvp-dnsmasq.pid"),
            phase: VmFounderPhase::Created,
        };

        // Matching id + name succeeds.
        let present = vec![VmInfo {
            id: UUID_0.into(),
            name: "vm-devnet-validator-0".into(),
            ..VmInfo::default()
        }];
        verify_resumed_founders_exist("192.0.2.50", &present, &[&recorded]).unwrap();

        // Missing VM ID fails and instructs operator to run `vm remove`.
        let err = verify_resumed_founders_exist("192.0.2.50", &[], &[&recorded]).unwrap_err();
        assert!(err.to_string().contains("vm remove"), "{err}");
        assert!(err.to_string().contains(UUID_0), "{err}");

        // Renamed VM ID also fails and instructs operator to run `vm remove`.
        let renamed = vec![VmInfo {
            id: UUID_0.into(),
            name: "someone-else".into(),
            ..VmInfo::default()
        }];
        let err = verify_resumed_founders_exist("192.0.2.50", &renamed, &[&recorded]).unwrap_err();
        assert!(err.to_string().contains("vm remove"), "{err}");
    }

    #[test]
    fn create_save_failure_triggers_best_effort_cleanup_and_reports_vm_id() {
        let dir = tempfile::tempdir().unwrap();
        // Pass a non-existent directory so VmState::save fails.
        let missing_dir = dir.path().join("does-not-exist");
        let mut state = VmState {
            version: VM_STATE_VERSION,
            network_name: "vm-devnet".into(),
            image_name: "outbe-vm-dev-0123456789abcdef".into(),
            os_image_hash: "0123456789abcdef".repeat(4),
            founders: Vec::new(),
        };
        let entry = VmFounderState {
            index: 0,
            name: "validator-0".into(),
            vm_name: "vm-devnet-validator-0".into(),
            vm_id: UUID_0.into(),
            vmm_host: Ssh {
                host: "192.0.2.50".into(),
                user: "ubuntu".into(),
                port: 22,
            },
            vmm_home: "~/.dstack-vmm".into(),
            bridge: "obr0".into(),
            bridge_ip: Ipv4Addr::new(10, 77, 0, 10),
            mac: String::new(),
            dhcp_hosts_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.hosts"),
            dhcp_leases_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.leases"),
            dhcp_pid_file: PathBuf::from("/run/outbe-mvp-dnsmasq.pid"),
            phase: VmFounderPhase::Created,
        };
        let mut removed_ids = Vec::new();
        let err = journal_created_vm(&missing_dir, &mut state, entry, |id| {
            removed_ids.push(id.to_owned());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(removed_ids, vec![UUID_0.to_owned()]);
        assert!(err.to_string().contains(UUID_0), "{err}");
        assert!(state.founders.is_empty());
    }

    #[test]
    fn name_collision_check_refuses_foreign_and_allows_resumed_vm() {
        let existing = vec![
            VmInfo {
                id: UUID_0.into(),
                name: "vm-devnet-validator-0".into(),
                ..VmInfo::default()
            },
            VmInfo {
                id: UUID_FOREIGN.into(),
                name: "vm-devnet-validator-1".into(),
                ..VmInfo::default()
            },
        ];
        // Without resumed state, both collide.
        let err = check_no_name_collision(
            "192.0.2.50",
            &existing,
            &["vm-devnet-validator-0"],
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("vm-devnet-validator-0"), "{err}");

        // With resumed state matching UUID_0, validator-0 is allowed while foreign validator-1 is refused.
        let resumed = BTreeMap::from([("vm-devnet-validator-0".to_owned(), UUID_0.to_owned())]);
        check_no_name_collision(
            "192.0.2.50",
            &existing,
            &["vm-devnet-validator-0"],
            &resumed,
        )
        .unwrap();
        let err = check_no_name_collision(
            "192.0.2.50",
            &existing,
            &["vm-devnet-validator-0", "vm-devnet-validator-1"],
            &resumed,
        )
        .unwrap_err();
        assert!(err.to_string().contains("vm-devnet-validator-1"), "{err}");
    }

    #[test]
    fn partial_state_journaling_supports_resume_and_blocks_complete_commands() {
        let dir = tempfile::tempdir().unwrap();
        let partial = VmState {
            version: VM_STATE_VERSION,
            network_name: "vm-devnet".into(),
            image_name: "outbe-vm-dev-0123456789abcdef".into(),
            os_image_hash: "0123456789abcdef".repeat(4),
            founders: vec![VmFounderState {
                index: 0,
                name: "validator-0".into(),
                vm_name: "vm-devnet-validator-0".into(),
                vm_id: UUID_0.into(),
                vmm_host: Ssh {
                    host: "192.0.2.50".into(),
                    user: "ubuntu".into(),
                    port: 22,
                },
                vmm_home: "~/.dstack-vmm".into(),
                bridge: "obr0".into(),
                bridge_ip: Ipv4Addr::new(10, 77, 0, 10),
                mac: String::new(),
                dhcp_hosts_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.hosts"),
                dhcp_leases_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.leases"),
                dhcp_pid_file: PathBuf::from("/run/outbe-mvp-dnsmasq.pid"),
                phase: VmFounderPhase::Created,
            }],
        };
        partial.save(dir.path()).unwrap();
        let loaded = VmState::load(dir.path()).unwrap();
        assert!(!loaded.is_complete(4));
        assert!(!loaded.is_complete(1));

        // Transitioning founder 0 to Ready with MAC completes a 1-founder state, still incomplete for 4.
        let mut ready = loaded;
        ready.founders[0].mac = "52:54:00:aa:bb:01".into();
        ready.founders[0].phase = VmFounderPhase::Ready;
        ready.save(dir.path()).unwrap();
        assert!(ready.is_complete(1));
        assert!(!ready.is_complete(4));
        assert_ne!(UUID_0, UUID_1);
    }

    #[tokio::test]
    async fn run_with_deadline_enforces_timeout_on_stalled_hyperlane_wait() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(25);
        let stalled = async {
            std::future::pending::<()>().await;
            Ok::<(), eyre::Report>(())
        };
        let outcome = run_with_deadline(deadline, stalled).await.unwrap();
        assert!(outcome.is_none(), "stalled setup must expire at deadline");

        let fast = async { Ok::<u32, eyre::Report>(42) };
        let fast_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        assert_eq!(
            run_with_deadline(fast_deadline, fast).await.unwrap(),
            Some(42)
        );
    }

    #[test]
    fn lag_assessment_flags_hardware_lagging_founder_and_majority_vrf() {
        let mk = |name: &str, finalized: u64, vrf: u64| VmFounderStatus {
            name: name.into(),
            vm_id: UUID_0.into(),
            bridge_ip: Ipv4Addr::new(10, 77, 0, 10),
            mac: "52:54:00:aa:bb:01".into(),
            vmm_status: "running".into(),
            vmm_running: true,
            chain_id: Some(424_242),
            finalized: Some(finalized),
            execution_peers: Some(3),
            consensus: Some(ConsensusStatus {
                connected_peers: 3,
                has_threshold_shares: true,
                last_finalized_block: finalized,
                vrf_material_version: vrf,
                last_dkg_activation_height: None,
                next_planned_activation_height: 0,
                randomness_status: None,
                is_validator: true,
                mode: "signer".into(),
                enclave: outbe_operator_node_role::rpc::EnclaveHealth {
                    state: "ready".into(),
                    ready: true,
                    offer_key_ready: true,
                },
            }),
            hyperlane_registered: Some(true),
            error: None,
        };

        // Hardware run 2 shape: founder 0 stuck at 1200 with vrf=0 while 1..3 are at 1360 with vrf=1.
        let statuses = vec![
            mk("validator-0", 1200, 0),
            mk("validator-1", 1360, 1),
            mk("validator-2", 1360, 1),
            mk("validator-3", 1360, 1),
        ];
        let assessed = assess_founders_lag(&statuses, 20);
        assert_eq!(assessed[0].lag, Some(160));
        assert!(assessed[0].lagging);
        assert!(assessed[0].reasons.iter().any(|r| r.contains("lag 160")));
        assert!(assessed[0]
            .reasons
            .iter()
            .any(|r| r.contains("vrf 0 != majority 1")));
        for item in &assessed[1..] {
            assert_eq!(item.lag, Some(0));
            assert!(!item.lagging);
        }

        // VRF-only mismatch is flagged even when finalized heights match.
        let vrf_only = vec![
            mk("validator-0", 1360, 0),
            mk("validator-1", 1360, 1),
            mk("validator-2", 1360, 1),
            mk("validator-3", 1360, 1),
        ];
        let assessed_vrf = assess_founders_lag(&vrf_only, 20);
        assert!(assessed_vrf[0].lagging);
        assert!(!assessed_vrf[1].lagging);

        // Healthy cohort within max_lag with unanimous VRF is not flagged.
        let healthy = vec![
            mk("validator-0", 1355, 1),
            mk("validator-1", 1360, 1),
            mk("validator-2", 1360, 1),
            mk("validator-3", 1360, 1),
        ];
        let assessed_ok = assess_founders_lag(&healthy, 20);
        assert!(assessed_ok.iter().all(|a| !a.lagging));
    }

    #[test]
    fn vm_is_gone_distinguishes_removing_from_removed() {
        let removing = GetInfoResponse {
            found: true,
            info: Some(VmInfo {
                id: UUID_0.into(),
                name: "vm-devnet-validator-0".into(),
                status: "removing".into(),
                ..VmInfo::default()
            }),
        };
        assert!(!vm_is_gone(&removing));

        let removed = GetInfoResponse {
            found: false,
            info: None,
        };
        assert!(vm_is_gone(&removed));
    }
}
