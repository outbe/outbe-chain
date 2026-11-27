//! Health, peer-count, VRF agreement, and block-rate helpers for rolling VM updates.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Result};
use outbe_operator_node_role::progress::Progress;
use outbe_operator_remote::HostExec;

use super::super::{
    assess_members_lag, fetch_host_vms, member_node_ready, query_members_and_orphans,
    render_vm_status, DesiredMember, MemberLagAssessment, RemoteOutbeUp, RemoteVmStatusOutput,
    UniqueHost, VmClassKind, VmMemberStatus,
};
use super::{NetworkSample, PollContext, PollStep};
use crate::network::{remote::NetworkArtifacts, vm_state::MemberRole};

#[cfg(not(test))]
const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(not(test))]
const BLOCK_RATE_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(test)]
const BLOCK_RATE_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

/// Conservative budget for guest boot + `outbe-vm-agent vm-boot` + TEE re-admission (`registerEnclave`)
/// + catch-up added to `stop_timeout` when computing the time-based DKG guard window.
const DKG_BOOT_AND_TEE_BUDGET_SECS: f64 = 60.0;
const DKG_GUARD_MARGIN_BLOCKS: u64 = 15;

pub(super) fn fetch_all_host_vms(
    unique_hosts: &BTreeMap<String, UniqueHost>,
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
) -> Result<BTreeMap<String, Vec<RemoteVmStatusOutput>>> {
    let mut host_vms_map = BTreeMap::new();
    for (hkey, host) in unique_hosts {
        let vms = fetch_host_vms(exec, &host.ssh, &host_bins[hkey], &host.vmm_home)?;
        host_vms_map.insert(hkey.clone(), vms);
    }
    Ok(host_vms_map)
}

pub(super) fn find_observed_vm<'a>(
    host_vms_map: &'a BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    hkey: &str,
    vm_name: &str,
) -> Option<&'a RemoteVmStatusOutput> {
    host_vms_map
        .get(hkey)
        .and_then(|vms| vms.iter().find(|vm| vm.name == vm_name))
}

pub(super) fn require_observed_vm(
    host_vms_map: &BTreeMap<String, Vec<RemoteVmStatusOutput>>,
    hkey: &str,
    member: &DesiredMember,
) -> Result<RemoteVmStatusOutput> {
    find_observed_vm(host_vms_map, hkey, &member.vm_name)
        .cloned()
        .ok_or_else(|| {
            eyre::eyre!(
                "VM `{}` missing on host {} during rolling update",
                member.vm_name,
                member.vmm_host.host
            )
        })
}

pub(super) fn validator_required_peers(members: &[DesiredMember]) -> usize {
    members
        .iter()
        .filter(|m| m.role != MemberRole::FullNode)
        .count()
        .saturating_sub(1)
}

pub(super) fn common_validator_vrf(statuses: &[VmMemberStatus]) -> Option<u64> {
    common_validator_vrf_excluding(statuses, None)
}

pub(super) fn common_validator_vrf_excluding(
    statuses: &[VmMemberStatus],
    exclude_idx: Option<usize>,
) -> Option<u64> {
    let validators: Vec<&VmMemberStatus> = statuses
        .iter()
        .enumerate()
        .filter(|(i, s)| Some(*i) != exclude_idx && s.role != MemberRole::FullNode)
        .map(|(_, s)| s)
        .collect();
    if validators.is_empty() {
        return Some(0);
    }
    let versions: BTreeSet<u64> = validators
        .iter()
        .filter_map(|s| s.vrf_material_version)
        .collect();
    let reported_count = validators
        .iter()
        .filter(|s| s.vrf_material_version.is_some())
        .count();
    if reported_count == validators.len() && versions.len() == 1 {
        versions.into_iter().next()
    } else {
        None
    }
}

pub(crate) fn all_validators_randomness_healthy(statuses: &[VmMemberStatus]) -> bool {
    statuses
        .iter()
        .filter(|s| s.role != MemberRole::FullNode)
        .all(|s| {
            s.consensus
                .as_ref()
                .and_then(|c| c.randomness_status.as_deref())
                .is_some_and(|st| st.eq_ignore_ascii_case("healthy"))
        })
}

/// Fail-closed pre-stop check requiring every validator to report consensus
/// `connectedPeers >= n - 1` (never accepting degraded `0 < connectedPeers < n - 1` or missing
/// `outbe_consensusStatus`). Note: `outbe_consensusStatus.connectedPeers` counts signers in the
/// last finalized certificate (`>= n - 1`, up to `n` when all validators sign before assembly).
pub(crate) fn check_validators_connected_peers(
    statuses: &[VmMemberStatus],
    required_peers: usize,
) -> Result<()> {
    let min_peers = required_peers as u64;
    for s in statuses.iter().filter(|s| s.role != MemberRole::FullNode) {
        let consensus = s.consensus.as_ref().ok_or_else(|| {
            eyre::eyre!(
                "refusing to stop member: validator `{}` is missing `outbe_consensusStatus` (fail-closed peer check)",
                s.name
            )
        })?;
        ensure!(
            consensus.connected_peers >= min_peers,
            "refusing to stop member: validator `{}` reports consensus connectedPeers={} (expected >= n-1={min_peers})",
            s.name,
            consensus.connected_peers
        );
    }
    Ok(())
}

pub(super) fn ensure_all_classified_ours(
    build: &NetworkArtifacts,
    sample: &NetworkSample,
    max_lag: u64,
    context: &str,
) -> Result<()> {
    let bad: Vec<String> = sample
        .statuses
        .iter()
        .filter(|s| s.classification != VmClassKind::Ours)
        .map(|s| format!("{}: classification={:?}", s.name, s.classification))
        .collect();
    if !bad.is_empty() {
        bail!(
            "{context} failed: {}\n\n{}",
            bad.join("; "),
            render_vm_status(build, &sample.statuses, &sample.orphans, max_lag)
        );
    }
    Ok(())
}

pub(super) fn ensure_all_members_healthy(
    build: &NetworkArtifacts,
    sample: &NetworkSample,
    required_peers: usize,
    max_lag: u64,
    context: &str,
) -> Result<()> {
    let mut problems = Vec::new();
    if !sample.orphans.is_empty() {
        problems.push(format!("{} orphan VM(s) present", sample.orphans.len()));
    }
    for (s, a) in sample.statuses.iter().zip(&sample.assessments) {
        if let Some(reason) = member_health_problem(s, a, build.config.chain_id, required_peers) {
            problems.push(reason);
        }
    }
    if sample.has_validators && sample.common_vrf.is_none() {
        problems.push("validators do not agree on a common vrf_material_version".to_owned());
    }
    if !problems.is_empty() {
        bail!(
            "{context} failed: {}\n\n{}",
            problems.join("; "),
            render_vm_status(build, &sample.statuses, &sample.orphans, max_lag)
        );
    }
    Ok(())
}

fn member_health_problem(
    s: &VmMemberStatus,
    a: &MemberLagAssessment,
    chain_id: u64,
    required_peers: usize,
) -> Option<String> {
    if s.classification != VmClassKind::Ours {
        return Some(format!("{}: classification={:?}", s.name, s.classification));
    }
    if !member_node_ready(s, chain_id, required_peers) {
        return Some(format!(
            "{}: {}",
            s.name,
            unready_member_reason(s, required_peers)
        ));
    }
    a.lagging
        .then(|| format!("{}: {}", s.name, a.reasons.join(", ")))
}

fn unready_member_reason(s: &VmMemberStatus, required_peers: usize) -> String {
    if let Some(err) = s.error.as_deref() {
        return err.to_owned();
    }
    if s.role == MemberRole::FullNode {
        return "not ready (vmm/rpc/consensus/tee)".to_owned();
    }
    match s.consensus.as_ref() {
        None => "missing `outbe_consensusStatus` (fail-closed peer check)".to_owned(),
        Some(c) if c.connected_peers < required_peers as u64 => format!(
            "consensus connectedPeers={} (expected >= n-1={required_peers})",
            c.connected_peers
        ),
        Some(_) => "not ready (vmm/rpc/consensus/tee)".to_owned(),
    }
}

/// Compute the effective DKG guard in finalized blocks from `stop_timeout`, a conservative
/// boot + TEE re-admission budget, the measured block rate, and a fixed safety margin, floored by
/// `floor_blocks`.
pub(crate) fn compute_effective_dkg_guard_blocks(
    stop_timeout: Duration,
    floor_blocks: u64,
    blocks_per_sec: f64,
) -> u64 {
    let safe_bps = if blocks_per_sec.is_finite() && blocks_per_sec > 0.0 {
        blocks_per_sec.clamp(0.1, 20.0)
    } else {
        1.0
    };
    let downtime_secs = stop_timeout.as_secs_f64() + DKG_BOOT_AND_TEE_BUDGET_SECS;
    let time_based = (downtime_secs * safe_bps).ceil() as u64 + DKG_GUARD_MARGIN_BLOCKS;
    time_based.max(floor_blocks)
}

pub(super) async fn measure_blocks_per_sec(
    ctx: &PollContext<'_>,
    initial_sample: &NetworkSample,
) -> Result<f64> {
    tokio::time::sleep(BLOCK_RATE_SAMPLE_INTERVAL).await;
    let second = sample_network(ctx).await?;
    let dt = second
        .sampled_at
        .saturating_duration_since(initial_sample.sampled_at)
        .as_secs_f64();
    let dh = second
        .max_finalized()
        .unwrap_or(0)
        .saturating_sub(initial_sample.max_finalized().unwrap_or(0));
    if dt > 0.0 && dh > 0 {
        Ok((dh as f64) / dt)
    } else {
        Ok(1.0)
    }
}

pub(super) async fn sample_network(ctx: &PollContext<'_>) -> Result<NetworkSample> {
    let (statuses, orphans) = query_members_and_orphans(
        ctx.build,
        ctx.desired_members,
        ctx.unique_hosts,
        ctx.host_bins,
        ctx.exec,
    )
    .await?;
    let sampled_at = Instant::now();
    let assessments = assess_members_lag(&statuses, ctx.max_lag);
    let all_ready = statuses.iter().zip(&assessments).all(|(s, a)| {
        s.classification == VmClassKind::Ours
            && member_node_ready(s, ctx.build.config.chain_id, ctx.required_peers)
            && !a.lagging
    });
    let has_validators = statuses.iter().any(|s| s.role != MemberRole::FullNode);
    let common_vrf = common_validator_vrf(&statuses);
    Ok(NetworkSample {
        sampled_at,
        statuses,
        assessments,
        orphans,
        all_ready,
        common_vrf,
        has_validators,
    })
}

pub(super) async fn poll_network_until<T, F>(
    ctx: &PollContext<'_>,
    deadline: Instant,
    progress: &mut Progress,
    mut step: F,
) -> Result<std::result::Result<T, NetworkSample>>
where
    F: FnMut(&NetworkSample) -> Result<PollStep<T>>,
{
    loop {
        let sample = sample_network(ctx).await?;
        match step(&sample)? {
            PollStep::Done(val) => return Ok(Ok(val)),
            PollStep::Continue(status_line) => {
                if Instant::now() >= deadline {
                    return Ok(Err(sample));
                }
                if !status_line.is_empty() {
                    progress.update(&status_line);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    tokio::time::sleep(remaining.min(UPDATE_POLL_INTERVAL)).await;
                }
            }
        }
    }
}
