//! Multi-host VM network status inspection (`outbe-net-up network status`).

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    sync::Arc,
    time::Duration,
};

use eyre::{bail, Context, Result};
use outbe_operator_node_role::{
    progress::{Progress, WatchDisplay},
    rpc::ConsensusStatus,
};
use outbe_operator_remote::HostExec;
use serde::Serialize;
use tokio::task::JoinSet;

use super::{
    ensure_vm_deployment, host_key,
    readiness::{assess_members_lag, member_node_ready, render_vm_status},
    remote_bin::{
        ensure_pinned_outbe_up_on_host, fetch_host_vms, resolve_state_or_bundled_outbe_up_sha256,
        RemoteOutbeUp,
    },
    rpc::{query_vm_rpc_batch, BatchedRpcResult},
    types::{
        build_all_desired_members, classify_host_vm, collect_unique_hosts,
        network_owner_tag_prefix, DesiredMember, RemoteVmStatusOutput, UniqueHost, VmClassKind,
        VmClassification,
    },
    StatusOptions,
};
use crate::network::{
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkVmState},
};

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
    pub(crate) hyperlane_healthy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hyperlane_controller: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hyperlane_active: Option<bool>,
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

    let state = {
        let lock = NetworkVmState::lock(options.artifacts)?;
        NetworkVmState::load_optional_locked(&lock, options.artifacts)?
    };
    if let Some(st) = &state {
        st.ensure_matches_network(&build)?;
    }
    let outbe_up_sha256 = resolve_state_or_bundled_outbe_up_sha256(&build.root, state.as_ref())?;

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
    let required_peers = desired_members
        .iter()
        .filter(|m| m.role != MemberRole::FullNode)
        .count()
        .saturating_sub(1);

    if options.json {
        print_status_json(
            &build,
            (&statuses, &assessments, &orphans),
            options.max_lag,
            required_peers,
        )?;
        return check_unhealthy_members(&statuses, &assessments, &orphans, required_peers);
    }

    if !options.watch {
        let mut progress = Progress::new();
        progress.done(&format!("queried {} VM member(s)", statuses.len()));
        println!(
            "\n{}",
            render_vm_status(&build, &statuses, &orphans, options.max_lag)
        );
        return check_unhealthy_members(&statuses, &assessments, &orphans, required_peers);
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

fn print_status_json(
    build: &NetworkArtifacts,
    (statuses, assessments, orphans): (
        &[VmMemberStatus],
        &[MemberLagAssessment],
        &[OrphanVmStatus],
    ),
    max_lag: u64,
    required_peers: usize,
) -> Result<()> {
    let healthy_count = statuses
        .iter()
        .zip(assessments)
        .filter(|(s, a)| {
            s.classification == VmClassKind::Ours
                && member_node_ready(s, build.config.chain_id, required_peers)
                && !a.lagging
        })
        .count();
    let members = statuses
        .iter()
        .zip(assessments)
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
        max_lag,
        healthy_count,
        total_count: statuses.len(),
        members,
        orphans,
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

pub(super) fn check_unhealthy_members(
    statuses: &[VmMemberStatus],
    assessments: &[MemberLagAssessment],
    orphans: &[OrphanVmStatus],
    required_peers: usize,
) -> Result<()> {
    let mut unhealthy: Vec<String> = statuses
        .iter()
        .zip(assessments)
        .filter_map(|(s, a)| member_unhealthy_reason(s, a, required_peers))
        .collect();
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

fn member_unhealthy_reason(
    status: &VmMemberStatus,
    assessment: &MemberLagAssessment,
    required_peers: usize,
) -> Option<String> {
    if status.classification != VmClassKind::Ours {
        return Some(format!("{}: {:?}", status.name, status.classification));
    }
    if !status.vmm_running || status.error.is_some() || status.finalized.is_none() {
        return Some(format!("{}: unreachable/not-running", status.name));
    }
    if status.role != MemberRole::FullNode && status.consensus.is_none() {
        return Some(format!("{}: missing outbe_consensusStatus", status.name));
    }
    if status.role != MemberRole::FullNode && status.hyperlane_healthy != Some(true) {
        return Some(format!("{}: Hyperlane unavailable/unhealthy", status.name));
    }
    if status.role != MemberRole::FullNode
        && status.consensus_peers.unwrap_or(0) < required_peers as u64
    {
        return Some(format!(
            "{}: consensus connectedPeers={} (expected >= n-1={required_peers})",
            status.name,
            status.consensus_peers.unwrap_or(0)
        ));
    }
    assessment
        .lagging
        .then(|| format!("{}: {}", status.name, assessment.reasons.join(", ")))
}

pub(crate) async fn query_members_and_orphans(
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
    let orphans = collect_orphan_vms(unique_hosts, &host_vms, &tag_prefix, &expected_names);

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

fn collect_orphan_vms(
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_vms: &BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    tag_prefix: &str,
    expected_names: &BTreeSet<&str>,
) -> Vec<OrphanVmStatus> {
    let mut orphans = Vec::new();
    for (hkey, vms) in host_vms {
        let host = &unique_hosts[hkey];
        for vm in vms {
            let is_orphan = vm
                .owner_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(tag_prefix))
                && !expected_names.contains(vm.name.as_str());
            if is_orphan {
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
    orphans
}

pub(super) fn build_member_status(
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
        return absent_member_status(member, kind, detail);
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

    let rpc = if vm.running && query_rpc && kind == VmClassKind::Ours {
        query_member_rpc(
            exec,
            &member,
            service_ip,
            boot_warning.as_deref(),
            &mut error,
        )
    } else {
        if !vm.running && error.is_none() {
            error.clone_from(&boot_warning);
        }
        BatchedRpcResult::default()
    };

    let tee_ready = (!effective_no_tee)
        .then(|| rpc.consensus.as_ref().map(ConsensusStatus::resident_ready))
        .flatten();
    let hyperlane = if vm.running
        && query_rpc
        && kind == VmClassKind::Ours
        && member.role != MemberRole::FullNode
    {
        super::query_hyperlane_health(exec, &member.vmm_host, &member.name, service_ip).ok()
    } else {
        None
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
        hyperlane_healthy: hyperlane.as_ref().map(|status| status.healthy),
        hyperlane_controller: hyperlane.as_ref().map(|status| status.controller.clone()),
        hyperlane_active: hyperlane.and_then(|status| status.active),
        consensus: rpc.consensus,
        boot_warning,
        error,
    }
}

fn absent_member_status(
    member: DesiredMember,
    kind: VmClassKind,
    detail: Option<String>,
) -> VmMemberStatus {
    VmMemberStatus {
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
        hyperlane_healthy: None,
        hyperlane_controller: None,
        hyperlane_active: None,
        consensus: None,
        boot_warning: None,
        error: Some("VM not created on host".to_owned()),
    }
}

fn query_member_rpc(
    exec: &dyn HostExec,
    member: &DesiredMember,
    service_ip: Ipv4Addr,
    boot_warning: Option<&str>,
    error: &mut Option<String>,
) -> BatchedRpcResult {
    let rpc = match query_vm_rpc_batch(
        exec,
        &member.vmm_host,
        &member.name,
        service_ip,
        member.rpc_port,
    ) {
        Ok(res) => res,
        Err(err) => {
            if error.is_none() {
                *error = Some(format_rpc_error_with_boot_warning(
                    format!(
                        "RPC unreachable on service IP {service_ip}:{}: {err:#}",
                        member.rpc_port
                    ),
                    boot_warning,
                ));
            }
            BatchedRpcResult::default()
        }
    };
    if (rpc.chain_id.is_none() || rpc.finalized.is_none()) && error.is_none() {
        *error = Some(format_rpc_error_with_boot_warning(
            format!(
                "RPC unreachable on service IP {service_ip}:{}",
                member.rpc_port
            ),
            boot_warning,
        ));
    }
    rpc
}

fn format_rpc_error_with_boot_warning(base: String, boot_warning: Option<&str>) -> String {
    match boot_warning {
        Some(warn) => format!("{base} (boot_error: {warn})"),
        None => base,
    }
}
