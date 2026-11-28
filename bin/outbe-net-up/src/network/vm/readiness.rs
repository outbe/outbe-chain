//! Readiness evaluation, lag assessment, and VM network status table rendering.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    status::{MemberLagAssessment, OrphanVmStatus, VmMemberStatus},
    types::VmClassKind,
};
use crate::network::{remote::NetworkArtifacts, vm_state::MemberRole};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadinessRequirements {
    pub(crate) chain_id: u64,
    pub(crate) required_peers: usize,
    pub(crate) baseline_b: Option<u64>,
    pub(crate) min_finalized_blocks: u64,
}

pub(crate) fn member_node_ready(
    status: &VmMemberStatus,
    chain_id: u64,
    required_peers: usize,
) -> bool {
    let base_ok = status.vmm_running
        && status.error.is_none()
        && status.chain_id == Some(chain_id)
        && status.finalized.is_some();
    if !base_ok {
        return false;
    }
    if status.role == MemberRole::FullNode {
        // Keyless full nodes sync from upstream/bootnodes without validator signing keys or TEE.
        return true;
    }
    validator_consensus_ready(status, required_peers)
}

fn validator_consensus_ready(status: &VmMemberStatus, required_peers: usize) -> bool {
    // Reth devp2p is not a full-mesh consensus layer (blocks arrive via Engine API); require at
    // least one execution peer when the network has peers, while strictly enforcing `>= n - 1`
    // on `consensus.connected_peers` below.
    let exec_ok = required_peers == 0 || status.execution_peers.unwrap_or(0) > 0;
    let Some(consensus) = status.consensus.as_ref() else {
        return false;
    };
    let core_ready = consensus.is_validator
        && consensus.mode == "signer"
        && consensus.has_threshold_shares
        && consensus.connected_peers >= required_peers as u64;
    let tee_ok = status.no_tee || consensus.resident_ready();
    let feeder_ok = status.feeder_healthy == Some(true);
    let hyperlane_ok = status.hyperlane_healthy == Some(true);
    exec_ok && core_ready && tee_ok && feeder_ok && hyperlane_ok
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
    req: ReadinessRequirements,
) -> bool {
    let Some(b) = req.baseline_b else {
        return false;
    };
    if statuses.is_empty() {
        return false;
    }
    let target_height = b.saturating_add(req.min_finalized_blocks);

    let all_nodes_ok = statuses.iter().zip(assessments).all(|(s, a)| {
        s.classification == VmClassKind::Ours
            && member_node_ready(s, req.chain_id, req.required_peers)
            && !a.lagging
            && s.finalized.is_some_and(|h| h >= target_height)
    });
    if !all_nodes_ok {
        return false;
    }

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
    let majority_vrf = majority_vrf_version(statuses);
    statuses
        .iter()
        .map(|s| assess_single_member_lag(s, highest_finalized, majority_vrf, max_lag))
        .collect()
}

fn majority_vrf_version(statuses: &[VmMemberStatus]) -> Option<u64> {
    let mut vrf_counts: BTreeMap<u64, usize> = BTreeMap::new();
    for v in statuses
        .iter()
        .filter(|s| s.role != MemberRole::FullNode)
        .filter_map(|s| s.vrf_material_version)
    {
        *vrf_counts.entry(v).or_insert(0) += 1;
    }
    vrf_counts
        .into_iter()
        .max_by_key(|(v, count)| (*count, *v))
        .map(|(v, _)| v)
}

fn assess_single_member_lag(
    status: &VmMemberStatus,
    highest_finalized: Option<u64>,
    majority_vrf: Option<u64>,
    max_lag: u64,
) -> MemberLagAssessment {
    let mut reasons = Vec::new();
    let lag = highest_finalized.zip(status.finalized).map(|(high, mine)| {
        let delta = high.saturating_sub(mine);
        if delta > max_lag {
            reasons.push(format!("lagging by {delta} blocks (max {max_lag})"));
        }
        delta
    });
    if status.role != MemberRole::FullNode {
        if let Some((maj, mine)) = majority_vrf.zip(status.vrf_material_version) {
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
}

pub(crate) fn render_vm_status(
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
        "{:<14} {:<10} {:<16} {:<10} {:<10} {:<8} {:<6} {:<6} {:<12} {:<10} {:<12} DETAIL",
        "MEMBER",
        "CLASS",
        "SERVICE_IP",
        "VMM",
        "MODE",
        "FINAL",
        "LAG",
        "VRF",
        "TEE",
        "FEEDER",
        "HYPERLANE"
    ));
    for (s, a) in statuses.iter().zip(&assessments) {
        lines.push(render_member_status_row(s, a));
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

fn render_member_status_row(s: &VmMemberStatus, a: &MemberLagAssessment) -> String {
    let class_str = format!("{:?}", s.classification).to_ascii_lowercase();
    let default_mode = if s.role == MemberRole::FullNode {
        "full-node"
    } else {
        "-"
    };
    let mode = s.consensus_mode.as_deref().unwrap_or(default_mode);
    let finalized = opt_u64_or_dash(s.finalized);
    let lag = opt_u64_or_dash(a.lag);
    let vrf = if s.role == MemberRole::FullNode {
        "-".to_owned()
    } else {
        opt_u64_or_dash(s.vrf_material_version)
    };
    let tee = format_member_tee(s);
    let feeder = format_member_feeder(s);
    let hyperlane = format_member_hyperlane(s);
    let detail = format_member_detail(s, a);
    format!(
        "{:<14} {:<10} {:<16} {:<10} {:<10} {:<8} {:<6} {:<6} {:<12} {:<10} {:<12} {detail}",
        s.name,
        class_str,
        s.service_ip,
        s.vmm_status,
        mode,
        finalized,
        lag,
        vrf,
        tee,
        feeder,
        hyperlane
    )
}

fn opt_u64_or_dash(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

fn format_member_tee(s: &VmMemberStatus) -> String {
    if s.role == MemberRole::FullNode {
        return "n/a(keyless)".to_owned();
    }
    if s.no_tee {
        return "skip(no-tee)".to_owned();
    }
    s.tee_ready
        .map(|r| if r { "ready" } else { "not-ready" }.to_owned())
        .unwrap_or_else(|| "-".to_owned())
}

fn format_member_feeder(s: &VmMemberStatus) -> &'static str {
    if s.role == MemberRole::FullNode {
        return "n/a";
    }
    match s.feeder_healthy {
        Some(true) => "ok",
        Some(false) => "unhealthy",
        None => "-",
    }
}

fn format_member_hyperlane(s: &VmMemberStatus) -> String {
    if s.role == MemberRole::FullNode {
        return "n/a".to_owned();
    }
    match (s.hyperlane_healthy, s.hyperlane_controller.as_deref()) {
        (Some(true), Some(controller)) => format!("ok/{controller}"),
        (Some(false), Some(controller)) => format!("bad/{controller}"),
        (Some(true), None) => "ok".to_owned(),
        (Some(false), None) => "bad".to_owned(),
        (None, _) => "-".to_owned(),
    }
}

fn format_member_detail(s: &VmMemberStatus, a: &MemberLagAssessment) -> String {
    let base = s
        .error
        .as_deref()
        .or(s.feeder_error.as_deref())
        .or(s.classification_detail.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if a.reasons.is_empty() {
                "ok".to_owned()
            } else {
                a.reasons.join(", ")
            }
        });
    match s.boot_warning.as_deref() {
        Some(warn) if !base.contains(warn) => format!("{base} (warn: {warn})"),
        _ => base,
    }
}
