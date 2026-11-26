//! Typed dstack VMM pRPC client over a pluggable transport (local Unix socket or SSH).

mod http;
mod ssh_command;
#[cfg(test)]
mod tests;
mod unix_transport;

use std::{collections::BTreeMap, time::Instant};

use eyre::{ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use self::{
    ssh_command::{
        build_prpc_curl_command, build_serial_log_command, parse_curl_http_response,
        remote_vmm_path,
    },
    unix_transport::LocalUnixVmmTransport,
};

/// Embedded `app-compose.json` for `key_provider: none`, no-KMS, no-gateway Outbe VMs.
pub const APP_COMPOSE_JSON: &str = include_str!("../../../../deploy/outbe-vm/app-compose.json");

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreateVmRequest {
    pub name: String,
    pub image: String,
    pub compose_file: String,
    pub vcpu: u32,
    /// Memory in MiB (dstack VMM expects MB).
    pub memory: u32,
    /// Disk size in GiB (dstack VMM expects GB).
    pub disk_size: u32,
    pub ports: Vec<PortMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    pub user_config: String,
    pub hugepages: bool,
    pub pin_numa: bool,
    pub stopped: bool,
    pub no_tee: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networking: Option<NetworkingConfig>,
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NetworkingConfig {
    pub mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bridge_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortMapping {
    pub protocol: String,
    pub host_port: u32,
    pub vm_port: u32,
    pub host_address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateVmRequest {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IdMessage {
    id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StatusRequest {
    pub ids: Vec<String>,
    pub brief: bool,
    pub keyword: String,
    pub page: u32,
    pub page_size: u32,
    pub status: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub vms: Vec<VmInfo>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VmmVersionResponse {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub rev: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VmmResourcesMeta {
    #[serde(default)]
    pub max_cvm_number: u32,
    #[serde(default)]
    pub max_allocable_vcpu: u32,
    #[serde(default)]
    pub max_allocable_memory_in_mb: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VmmNetworkingMeta {
    #[serde(default)]
    pub supported_modes: Vec<String>,
    #[serde(default)]
    pub default_mode: String,
    #[serde(default)]
    pub default_bridge: String,
    #[serde(default)]
    pub max_queues: u32,
    #[serde(default)]
    pub default_vhost: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VmmMetaResponse {
    #[serde(default)]
    pub resources: VmmResourcesMeta,
    #[serde(default)]
    pub networking: VmmNetworkingMeta,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GetInfoResponse {
    #[serde(default)]
    pub found: bool,
    #[serde(default)]
    pub info: Option<VmInfo>,
}

impl GetInfoResponse {
    /// Return true when dstack VMM reports that this VM does not exist (`found == false` or `info == null`).
    pub fn is_gone(&self) -> bool {
        !self.found || self.info.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VmConfigurationSummary {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub vcpu: u32,
    #[serde(default)]
    pub memory: u32,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub stopped: Option<bool>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VmInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub uptime: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub exited_at: Option<String>,
    #[serde(default)]
    pub boot_progress: String,
    #[serde(default)]
    pub boot_error: String,
    #[serde(default)]
    pub shutdown_progress: String,
    #[serde(default)]
    pub configuration: Option<VmConfigurationSummary>,
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    #[serde(default)]
    pub interfaces: Vec<NetworkInterfaceStatus>,
    #[serde(default)]
    pub running: bool,
}

impl VmInfo {
    /// Exact runtime status strings returned by dstack VMM (`vm_runtime_status`).
    pub const STATUS_RUNNING: &'static str = "running";
    /// Guest exited while `.started` remained set (the state reached after graceful `ShutdownVm`).
    pub const STATUS_EXITED: &'static str = "exited";
    pub const STATUS_STOPPING: &'static str = "stopping";
    pub const STATUS_STOPPED: &'static str = "stopped";
    pub const STATUS_REMOVING: &'static str = "removing";

    /// Return true when QEMU is not running and dstack reports a terminal stopped status.
    ///
    /// In dstack VMM (`vm_runtime_status`), a created-stopped or force-stopped VM reports
    /// `"stopped"`, whereas a VM powered off gracefully by `ShutdownVm` reports `"exited"`.
    pub fn is_stopped(&self) -> bool {
        !self.running
            && matches!(
                self.status.as_str(),
                Self::STATUS_STOPPED | Self::STATUS_EXITED
            )
    }

    /// Return true only when dstack reports the VM is administratively stopped (`.started == false`
    /// and QEMU not running), so `auto_restart` will not relaunch it.
    pub fn is_administratively_stopped(&self) -> bool {
        !self.running && self.status == Self::STATUS_STOPPED
    }

    /// Verify that this VM is stopped and has never been started, making initial `hda.img`
    /// substitution safe.
    pub fn ensure_never_started_and_stopped(&self) -> Result<()> {
        ensure!(
            !self.running && self.status == Self::STATUS_STOPPED,
            "refusing to stage hda.img for VM `{}` ({}): VM must be stopped and never started (status={}, running={})",
            self.name,
            self.id,
            self.status,
            self.running
        );
        ensure!(
            self.boot_progress.is_empty(),
            "refusing to stage hda.img for VM `{}` ({}): VM has already booted (boot_progress={})",
            self.name,
            self.id,
            self.boot_progress
        );
        ensure!(
            self.events.is_empty(),
            "refusing to stage hda.img for VM `{}` ({}): VM already has {} guest event(s) indicating prior boot",
            self.name,
            self.id,
            self.events.len()
        );
        let exited_at_clean = self
            .exited_at
            .as_deref()
            .is_none_or(|v| v.is_empty() || v == "never");
        ensure!(
            exited_at_clean,
            "refusing to stage hda.img for VM `{}` ({}): VM has prior exit timestamp ({:?})",
            self.name,
            self.id,
            self.exited_at
        );
        if let Some(cfg) = &self.configuration {
            ensure!(
                cfg.stopped != Some(false),
                "refusing to stage hda.img for VM `{}` ({}): VMM manifest records started state (configuration.stopped=false)",
                self.name,
                self.id
            );
        }
        Ok(())
    }

    /// Return the configured guest image name reported by VMM, if present.
    pub fn image_name(&self) -> Option<&str> {
        self.configuration
            .as_ref()
            .map(|cfg| cfg.image.as_str())
            .filter(|name| !name.is_empty())
    }

    /// Return one VMM manifest annotation value, if present in `configuration.annotations`.
    pub fn annotation(&self, key: &str) -> Option<&str> {
        self.configuration
            .as_ref()
            .and_then(|cfg| cfg.annotations.get(key))
            .map(String::as_str)
    }

    /// Return the effective `app_id` from `configuration.app_id` or top-level `VmInfo.app_id`.
    pub fn effective_app_id(&self) -> Option<&str> {
        self.configuration
            .as_ref()
            .and_then(|cfg| cfg.app_id.as_deref())
            .filter(|s| !s.is_empty())
            .or_else(|| (!self.app_id.is_empty()).then_some(self.app_id.as_str()))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct NetworkInterfaceStatus {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub mac: String,
}

/// Transport used by `VmmClient` to call dstack VMM pRPC endpoints and read serial logs.
pub trait VmmTransport {
    /// Send a JSON `POST /prpc/<method>?json` request and return the response body string.
    fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String>;

    /// Send a JSON `POST /prpc/<method>?json` request bounded by an overall total `deadline`.
    fn prpc_post_with_deadline(
        &self,
        method: &str,
        payload: &[u8],
        deadline: Option<Instant>,
    ) -> Result<String> {
        let _ = deadline;
        self.prpc_post(method, payload)
    }

    /// Read the last `lines` lines of the VM's serial console log.
    fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String>;
}

#[derive(Clone)]
pub struct VmmClient<T> {
    transport: T,
    label: String,
}

impl<T: VmmTransport> VmmClient<T> {
    pub fn new(transport: T, label: impl Into<String>) -> Self {
        Self {
            transport,
            label: label.into(),
        }
    }

    pub fn create_vm(&self, request: &CreateVmRequest) -> Result<String> {
        self.create_vm_with_deadline(request, None)
    }

    pub fn create_vm_with_deadline(
        &self,
        request: &CreateVmRequest,
        deadline: Option<Instant>,
    ) -> Result<String> {
        let response: IdMessage = self.prpc_call("CreateVm", request, deadline)?;
        validate_vm_id(&response.id)?;
        Ok(response.id)
    }

    pub fn start_vm(&self, vm_id: &str) -> Result<()> {
        self.start_vm_with_deadline(vm_id, None)
    }

    pub fn start_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StartVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    /// Update an existing VM's manifest in-place via dstack VMM `UpdateVm` (e.g. switch `image`
    /// while keeping the same VM UUID, MAC, networking, annotations, and `hda.img` data disk).
    pub fn update_vm(&self, request: &UpdateVmRequest) -> Result<()> {
        self.update_vm_with_deadline(request, None)
    }

    pub fn update_vm_with_deadline(
        &self,
        request: &UpdateVmRequest,
        deadline: Option<Instant>,
    ) -> Result<()> {
        validate_vm_id(&request.id)?;
        let _: serde_json::Value = self.prpc_call("UpdateVm", request, deadline)?;
        Ok(())
    }

    /// Request a graceful guest OS poweroff via dstack VMM `ShutdownVm`.
    ///
    /// In dstack VMM, `ShutdownVm` forwards to the guest agent (`systemctl poweroff`) and never
    /// falls back to a forced kill; once QEMU exits, VMM reports `running = false`, `status = "exited"`.
    pub fn shutdown_vm(&self, vm_id: &str) -> Result<()> {
        self.shutdown_vm_with_deadline(vm_id, None)
    }

    pub fn shutdown_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "ShutdownVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    /// Force-stop a VM via dstack VMM `StopVm` (terminates QEMU without waiting for guest poweroff,
    /// clears `.started`, and releases TAP interfaces).
    pub fn stop_vm(&self, vm_id: &str) -> Result<()> {
        self.stop_vm_with_deadline(vm_id, None)
    }

    pub fn stop_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StopVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    pub fn remove_vm(&self, vm_id: &str) -> Result<()> {
        self.remove_vm_with_deadline(vm_id, None)
    }

    pub fn remove_vm_with_deadline(&self, vm_id: &str, deadline: Option<Instant>) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "RemoveVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        Ok(())
    }

    pub fn status(&self, request: &StatusRequest) -> Result<StatusResponse> {
        self.status_with_deadline(request, None)
    }

    pub fn status_with_deadline(
        &self,
        request: &StatusRequest,
        deadline: Option<Instant>,
    ) -> Result<StatusResponse> {
        let response: StatusResponse = self.prpc_call("Status", request, deadline)?;
        for vm in &response.vms {
            if !vm.id.is_empty() {
                validate_vm_id(&vm.id)?;
            }
        }
        Ok(response)
    }

    pub fn get_info(&self, vm_id: &str) -> Result<GetInfoResponse> {
        self.get_info_with_deadline(vm_id, None)
    }

    pub fn get_info_with_deadline(
        &self,
        vm_id: &str,
        deadline: Option<Instant>,
    ) -> Result<GetInfoResponse> {
        validate_vm_id(vm_id)?;
        let response: GetInfoResponse = self.prpc_call(
            "GetInfo",
            &IdMessage {
                id: vm_id.to_owned(),
            },
            deadline,
        )?;
        if let Some(info) = &response.info {
            if !info.id.is_empty() {
                validate_vm_id(&info.id)?;
            }
        }
        Ok(response)
    }

    pub fn get_meta(&self) -> Result<serde_json::Value> {
        self.prpc_call("GetMeta", &serde_json::json!({}), None)
    }

    pub fn get_meta_typed(&self) -> Result<VmmMetaResponse> {
        self.prpc_call("GetMeta", &serde_json::json!({}), None)
    }

    pub fn version(&self) -> Result<VmmVersionResponse> {
        self.prpc_call("Version", &serde_json::json!({}), None)
    }

    /// Read the tail of the VM serial log from `<vmm_home>/vm/<vm_id>/serial.log` (or VMM `/logs`).
    pub fn serial_log_tail(&self, vm_id: &str, lines: usize) -> Result<String> {
        validate_vm_id(vm_id)?;
        self.transport.read_serial_log(vm_id, lines)
    }

    fn prpc_call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
        deadline: Option<Instant>,
    ) -> Result<Resp> {
        let payload = serde_json::to_vec(request)?;
        let body = self
            .transport
            .prpc_post_with_deadline(method, &payload, deadline)
            .wrap_err_with(|| format!("VMM pRPC {method} failed on {}", self.label))?;
        if body.trim().is_empty() {
            return serde_json::from_str("{}")
                .wrap_err_with(|| format!("VMM pRPC {method} returned empty body"));
        }
        serde_json::from_str(&body)
            .wrap_err_with(|| format!("invalid JSON from VMM pRPC {method}: {}", body.trim()))
    }
}

/// Validate that `vm_id` is a canonical 36-character UUID (`8-4-4-4-12` hex).
pub fn validate_vm_id(vm_id: &str) -> Result<()> {
    let bytes = vm_id.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(idx, byte)| {
            if matches!(idx, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    ensure!(valid, "invalid VM ID (expected canonical UUID): {vm_id}");
    Ok(())
}

/// Compute the dstack `app_id` (first 40 lowercase hex chars of SHA-256 of `compose_file`).
pub fn calc_app_id(compose_file: &str) -> String {
    let digest = Sha256::digest(compose_file.as_bytes());
    hex::encode(digest)[..40].to_owned()
}

/// Extract the primary NIC MAC address from VMM `VmInfo`.
pub fn extract_primary_mac(info: &VmInfo) -> Result<String> {
    let selected = info
        .interfaces
        .iter()
        .find(|iface| iface.mode == "bridge" && !iface.mac.is_empty())
        .or_else(|| info.interfaces.iter().find(|iface| !iface.mac.is_empty()))
        .ok_or_else(|| {
            eyre::eyre!(
                "VMM did not report a network interface MAC for VM {} ({})",
                info.name,
                info.id
            )
        })?;
    normalize_mac(&selected.mac)
}

/// Validate and lowercase a colon-separated 6-octet MAC address.
fn normalize_mac(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let octets: Vec<&str> = trimmed.split(':').collect();
    ensure!(
        octets.len() == 6
            && octets
                .iter()
                .all(|octet| octet.len() == 2 && octet.bytes().all(|b| b.is_ascii_hexdigit())),
        "invalid MAC address from VMM: {raw}"
    );
    Ok(trimmed)
}
