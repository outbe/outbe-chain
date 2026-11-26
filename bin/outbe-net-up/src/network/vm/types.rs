//! Strict remote `outbe-up --json` DTOs, desired member model, and host VM classification.

use std::{collections::BTreeMap, net::Ipv4Addr};

use eyre::Result;
use outbe_operator_remote::Ssh;
use serde::{Deserialize, Serialize};

use super::host_key;
use crate::network::{
    config::{ResolvedFounderVm, VmFlavor},
    remote::NetworkArtifacts,
    vm_state::{MemberRole, NetworkMemberState, NetworkVmState},
};

#[derive(Debug, Deserialize)]
pub(super) struct RemoteVersionOutput {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) binary_sha256: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteHostStatusOutput {
    pub(crate) ready_for_vm: bool,
    #[serde(default)]
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteImagePublishOutput {
    pub(crate) image_name: String,
    pub(crate) os_image_hash: String,
    pub(crate) target_dir: String,
    #[allow(dead_code)]
    pub(crate) reused: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteListedImage {
    pub(crate) name: String,
    pub(crate) os_image_hash: String,
    pub(crate) path: String,
    pub(crate) has_archive_manifest: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteDiskPrepareOutput {
    #[allow(dead_code)]
    pub(crate) output: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) staging_name: Option<String>,
    #[allow(dead_code)]
    pub(crate) size_gib: u32,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RemoteStagedDiskInfo {
    pub(crate) staging_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) sha256: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) size_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteCleanStagingOutput {
    #[allow(dead_code)]
    pub(crate) staging_name: String,
    #[allow(dead_code)]
    pub(crate) path: String,
    #[allow(dead_code)]
    pub(crate) removed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteCleanStaleOutput {
    #[allow(dead_code)]
    pub(crate) staging_root: String,
    #[serde(default)]
    pub(crate) removed: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteVmCreateOutput {
    pub(crate) name: String,
    pub(crate) vm_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) owner_tag: Option<String>,
    pub(crate) image_name: String,
    pub(crate) os_image_hash: String,
    pub(crate) mac: String,
    #[allow(dead_code)]
    pub(crate) bridge: String,
    #[allow(dead_code)]
    pub(crate) phase: String,
    #[serde(default)]
    pub(crate) staging_consumed: bool,
    #[serde(default)]
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteVmStartOutput {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) vm_id: String,
    #[allow(dead_code)]
    pub(crate) already_running: bool,
    #[allow(dead_code)]
    pub(crate) vmm_status: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteVmStopOutput {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) vm_id: String,
    #[allow(dead_code)]
    pub(crate) already_stopped: bool,
    #[allow(dead_code)]
    pub(crate) forced: bool,
    #[allow(dead_code)]
    pub(crate) vmm_status: String,
    #[serde(default)]
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteVmUpdateOutput {
    pub(crate) vm_id: String,
    pub(crate) image_name: String,
    pub(crate) os_image_hash: String,
    pub(crate) already_updated: bool,
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
pub(crate) struct RemoteVmLogsOutput {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) vm_id: String,
    #[allow(dead_code)]
    pub(crate) lines: usize,
    pub(crate) log: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteVmRemoveOutput {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) vm_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) forgot_intent: bool,
    #[serde(default)]
    pub(crate) warning: Option<String>,
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
    pub(crate) rpc_port: u16,
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

pub(crate) struct UniqueHost {
    pub(crate) ssh: Ssh,
    pub(crate) vmm_home: String,
    pub(crate) bridge: String,
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
        return VmClassification::Foreign {
            observed: Box::new(vm.clone()),
            reason: format_foreign_owner_reason(vm, &desired.owner_tag),
        };
    }

    let mut drifts = collect_vm_drifts(desired, vm);
    if vm.phase != "ready" {
        return if drifts.is_empty() {
            VmClassification::Partial {
                observed: Box::new(vm.clone()),
            }
        } else {
            VmClassification::Stale {
                observed: Box::new(vm.clone()),
                reasons: drifts,
            }
        };
    }

    if vm.vmm_status == "missing" {
        drifts.push("VMM reports VM missing".to_owned());
    }
    if !drifts.is_empty() {
        return VmClassification::Stale {
            observed: Box::new(vm.clone()),
            reasons: drifts,
        };
    }

    VmClassification::Ours {
        observed: Box::new(vm.clone()),
    }
}

fn format_foreign_owner_reason(vm: &RemoteVmStatusOutput, expected_tag: &str) -> String {
    match vm.owner_tag.as_deref() {
        Some(other) => format!(
            "owner_tag mismatch: host VM `{}` has tag `{other}`, expected `{expected_tag}`",
            vm.name
        ),
        None => format!(
            "host VM `{}` has no owner_tag (unmanaged or pre-S6.3 VM), expected `{expected_tag}`",
            vm.name
        ),
    }
}

fn collect_vm_drifts(desired: &DesiredMember, vm: &RemoteVmStatusOutput) -> Vec<String> {
    let mut drifts = Vec::new();
    let observed_service_ip = vm.service_ip.or(vm.intended_ip);
    let checks: [(bool, String); 6] = [
        (
            vm.bridge != desired.bridge,
            format!("bridge (`{}` vs `{}`)", vm.bridge, desired.bridge),
        ),
        (
            observed_service_ip != Some(desired.service_ip),
            format!(
                "service_ip ({observed_service_ip:?} vs {})",
                desired.service_ip
            ),
        ),
        (
            vm.vcpu != desired.vcpu,
            format!("vcpu ({} vs {})", vm.vcpu, desired.vcpu),
        ),
        (
            vm.memory_gib != desired.memory_gib,
            format!("memory_gib ({} vs {})", vm.memory_gib, desired.memory_gib),
        ),
        (
            vm.disk_gib != desired.disk_gib,
            format!("disk_gib ({} vs {})", vm.disk_gib, desired.disk_gib),
        ),
        (
            vm.no_tee != desired.no_tee,
            format!("no_tee ({} vs {})", vm.no_tee, desired.no_tee),
        ),
    ];
    for (drifted, desc) in checks {
        if drifted {
            drifts.push(desc);
        }
    }
    if let Some(expected_disk) = desired.recorded_disk_sha256.as_deref() {
        if vm.disk_source_sha256.as_deref() != Some(expected_disk) {
            drifts.push(format!(
                "disk_source_sha256 ({:?} vs `{expected_disk}`)",
                vm.disk_source_sha256
            ));
        }
    }
    if vm_image_mismatches(desired, vm) {
        drifts.push(format!(
            "image (`{}`/{} vs {:?}/{:?})",
            vm.image_name, vm.os_image_hash, desired.image_name, desired.image_hash
        ));
    }
    drifts
}

fn vm_image_mismatches(desired: &DesiredMember, vm: &RemoteVmStatusOutput) -> bool {
    let hash_mismatch = desired
        .image_hash
        .as_deref()
        .is_some_and(|h| vm.os_image_hash != h);
    let name_mismatch = desired
        .image_name
        .as_deref()
        .is_some_and(|img| vm.image_name != img);
    hash_mismatch || name_mismatch
}

pub(crate) fn collect_unique_hosts(members: &[DesiredMember]) -> BTreeMap<String, UniqueHost> {
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

pub(crate) fn build_member_state(
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
        rpc_port: member.rpc_port,
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

pub(crate) fn build_desired_members(
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
        let (eff_image_name, eff_image_hash) =
            resolve_member_image(state, prev, image_name, image_hash);
        out.push(DesiredMember {
            index,
            role: MemberRole::Founder,
            name: founder.name.clone(),
            vm_name,
            owner_tag,
            vmm_host: resolved.vmm_host,
            vmm_home: resolved.vmm_home,
            bridge: resolved.bridge,
            service_ip: resolved.vm_ip,
            advertise_ip: founder.advertise_ip,
            rpc_port: founder.rpc_port(),
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

fn resolve_member_image(
    state: Option<&NetworkVmState>,
    prev: Option<&NetworkMemberState>,
    image_name: Option<&str>,
    image_hash: Option<&str>,
) -> (Option<String>, Option<String>) {
    let eff_image_hash = image_hash
        .map(ToOwned::to_owned)
        .or_else(|| {
            prev.filter(|m| !m.image_hash.is_empty())
                .map(|m| m.image_hash.clone())
        })
        .or_else(|| {
            state
                .filter(|st| !st.image_hash.is_empty())
                .map(|st| st.image_hash.clone())
        });
    let eff_image_name = image_name.map(ToOwned::to_owned).or_else(|| {
        state
            .filter(|st| {
                !st.image_name.is_empty()
                    && eff_image_hash.as_deref().is_none_or(|h| h == st.image_hash)
            })
            .map(|st| st.image_name.clone())
    });
    (eff_image_name, eff_image_hash)
}

/// Build desired members for all roles: founders from `network.yaml` plus any added
/// `joining-validator` and `full-node` members recorded in `network-vm-state.json`.
pub(crate) fn build_all_desired_members(
    build: &NetworkArtifacts,
    state: Option<&NetworkVmState>,
    no_tee_override: Option<bool>,
    image_name: Option<&str>,
    image_hash: Option<&str>,
) -> Result<Vec<DesiredMember>> {
    let mut out = build_desired_members(build, state, no_tee_override, image_name, image_hash)?;
    let Some(st) = state else {
        return Ok(out);
    };
    for m in &st.members {
        if m.role == MemberRole::Founder || out.iter().any(|existing| existing.vm_name == m.vm_name)
        {
            continue;
        }
        let eff_image_name = (!st.image_name.is_empty() && st.image_hash == m.image_hash)
            .then(|| st.image_name.clone());
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
            rpc_port: m.rpc_port,
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
    Ok(out)
}

pub(crate) fn can_reuse_staged_disk(
    entry: Option<&RemoteStagedDiskInfo>,
    expected_sha256: Option<&str>,
) -> bool {
    let (Some(entry), Some(expected)) = (entry, expected_sha256) else {
        return false;
    };
    entry.error.is_none() && entry.sha256.as_deref() == Some(expected)
}
