//! Read-only views of locally recorded VMs: status (with DHCP lease lookup) and serial logs.

use std::{fs, net::Ipv4Addr, path::Path};

use eyre::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::{
    state::{validate_expected_vm_id, verify_expected_vm_id},
    LocalVmPhase, LocalVmState, LocalVmStore,
};
use crate::{
    dnsmasq::lookup_lease_ip_by_mac,
    vmm::{extract_primary_mac, VmInfo, VmmClient, VmmTransport},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalVmStatusOutput {
    pub name: String,
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub phase: LocalVmPhase,
    pub vmm_status: String,
    pub running: bool,
    pub uptime: String,
    pub mac: String,
    pub ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp_ip: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    pub image_name: String,
    pub os_image_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_source_sha256: Option<String>,
    pub app_id: String,
    pub no_tee: bool,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LocalVmStatusOutput {
    /// Status built from the journal alone: not running, IP from the intended service IP only.
    fn from_journal(state: &LocalVmState, vmm_status: &str, error: String) -> Self {
        Self {
            name: state.name.clone(),
            vm_id: state.vm_id.clone(),
            owner_tag: state.owner_tag.clone(),
            phase: state.phase,
            vmm_status: vmm_status.to_owned(),
            running: false,
            uptime: String::new(),
            mac: state.mac.clone(),
            ip: state.intended_ip,
            service_ip: state.intended_ip,
            dhcp_ip: None,
            intended_ip: state.intended_ip,
            image_name: state.image_name.clone(),
            os_image_hash: state.os_image_hash.clone(),
            disk_source_sha256: state.disk_source_sha256.clone(),
            app_id: state.app_id.clone(),
            no_tee: state.no_tee,
            vcpu: state.vcpu,
            memory_gib: state.memory_gib,
            disk_gib: state.disk_gib,
            bridge: state.bridge.clone(),
            boot_error: None,
            error: Some(error),
        }
    }

    /// Record a DHCP lease IP. The intended service IP keeps priority for `ip`.
    fn with_dhcp_ip(mut self, dhcp_ip: Option<Ipv4Addr>) -> Self {
        self.dhcp_ip = dhcp_ip;
        self.ip = self.service_ip.or(dhcp_ip);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalVmLogsOutput {
    pub name: String,
    pub vm_id: String,
    pub lines: usize,
    pub log: String,
}

/// Query status for one locally recorded VM, reading DHCP leases best-effort for IP lookup.
pub fn status_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    dhcp_leases_path: &Path,
) -> Result<LocalVmStatusOutput> {
    status_local_vm_with_expected_id(store, vmm, name, None, dhcp_leases_path)
}

/// Same as [`status_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock
/// before issuing any VMM RPC.
pub fn status_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    dhcp_leases_path: &Path,
) -> Result<LocalVmStatusOutput> {
    validate_expected_vm_id(expect_vm_id)?;
    let guard = store.lock_vm(name)?;
    let state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    let leases_content = fs::read_to_string(dhcp_leases_path).ok();
    query_status_from_state(vmm, &state, leases_content.as_deref())
}

/// Query status for all locally recorded VMs in `store`.
pub fn status_all_local_vms<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    dhcp_leases_path: &Path,
) -> Result<Vec<LocalVmStatusOutput>> {
    let names = store.list_vm_names()?;
    let leases_content = fs::read_to_string(dhcp_leases_path).ok();
    let mut results = Vec::with_capacity(names.len());
    for name in names {
        let guard = store.lock_vm(&name)?;
        if let Some(state) = store.load_optional_locked(&guard, &name)? {
            results.push(query_status_from_state(
                vmm,
                &state,
                leases_content.as_deref(),
            )?);
        }
    }
    Ok(results)
}

fn query_status_from_state<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    leases_content: Option<&str>,
) -> Result<LocalVmStatusOutput> {
    if state.is_unresolved_creating() {
        return Ok(LocalVmStatusOutput::from_journal(
            state,
            "creating",
            "VM creation was interrupted before VMM ID was recorded; resume `vm create` or clean up with `vm remove`"
                .to_owned(),
        ));
    }

    let lease_ip = |mac: &str| leases_content.and_then(|raw| lookup_lease_ip_by_mac(raw, mac));
    let resp = vmm.get_info(&state.vm_id)?;
    if resp.is_gone() {
        let missing = LocalVmStatusOutput::from_journal(
            state,
            "missing",
            format!("VM {} not found on local VMM", state.vm_id),
        );
        return Ok(missing.with_dhcp_ip(lease_ip(&state.mac)));
    }

    let info = resp.info.unwrap();
    let mac = extract_primary_mac(&info).unwrap_or_else(|_| state.mac.clone());
    let dhcp_ip = lease_ip(&mac);
    Ok(live_status(state, info, mac).with_dhcp_ip(dhcp_ip))
}

/// Status of a VM that the VMM still reports, with the VMM view of runtime fields.
fn live_status(state: &LocalVmState, info: VmInfo, mac: String) -> LocalVmStatusOutput {
    let image_name = info
        .image_name()
        .map(str::to_owned)
        .unwrap_or_else(|| state.image_name.clone());
    let error = (info.name != state.name).then(|| {
        format!(
            "VMM reports name `{}` for {}, expected `{}`",
            info.name, state.vm_id, state.name
        )
    });
    LocalVmStatusOutput {
        vmm_status: info.status,
        running: info.running,
        uptime: info.uptime,
        mac,
        image_name,
        boot_error: (!info.boot_error.is_empty()).then_some(info.boot_error),
        error,
        ..LocalVmStatusOutput::from_journal(state, "", String::new())
    }
}

/// Read the tail of the VM serial console log for one locally recorded VM.
pub fn logs_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    lines: usize,
) -> Result<LocalVmLogsOutput> {
    logs_local_vm_with_expected_id(store, vmm, name, None, lines)
}

/// Same as [`logs_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock before
/// reading the serial log.
pub fn logs_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    lines: usize,
) -> Result<LocalVmLogsOutput> {
    validate_expected_vm_id(expect_vm_id)?;
    let guard = store.lock_vm(name)?;
    let state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase != LocalVmPhase::Creating && !state.vm_id.is_empty(),
        "VM `{name}` has no recorded VMM ID yet (phase: {:?})",
        state.phase
    );
    let log = vmm.serial_log_tail(&state.vm_id, lines)?;
    Ok(LocalVmLogsOutput {
        name: state.name,
        vm_id: state.vm_id,
        lines,
        log,
    })
}
