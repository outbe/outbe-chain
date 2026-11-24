//! Typed dstack VMM pRPC client over SSH Unix-socket transport.

use std::collections::BTreeMap;

use eyre::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::network::{
    config::Ssh,
    remote::{shell_quote, SshOptions},
};

/// Embedded `app-compose.json` for `key_provider: none`, no-KMS, no-gateway Outbe VMs.
pub(crate) const APP_COMPOSE_JSON: &str =
    include_str!("../../../../deploy/outbe-vm/app-compose.json");

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CreateVmRequest {
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) compose_file: String,
    pub(crate) vcpu: u32,
    /// Memory in MiB (dstack VMM expects MB).
    pub(crate) memory: u32,
    /// Disk size in GiB (dstack VMM expects GB).
    pub(crate) disk_size: u32,
    pub(crate) ports: Vec<PortMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) app_id: Option<String>,
    pub(crate) user_config: String,
    pub(crate) hugepages: bool,
    pub(crate) pin_numa: bool,
    pub(crate) stopped: bool,
    pub(crate) no_tee: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) networking: Option<NetworkingConfig>,
    pub(crate) annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct NetworkingConfig {
    pub(crate) mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) bridge_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PortMapping {
    pub(crate) protocol: String,
    pub(crate) host_port: u32,
    pub(crate) vm_port: u32,
    pub(crate) host_address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdMessage {
    pub(crate) id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct StatusRequest {
    pub(crate) ids: Vec<String>,
    pub(crate) brief: bool,
    pub(crate) keyword: String,
    pub(crate) page: u32,
    pub(crate) page_size: u32,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct StatusResponse {
    #[serde(default)]
    pub(crate) vms: Vec<VmInfo>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct GetInfoResponse {
    #[serde(default)]
    pub(crate) found: bool,
    #[serde(default)]
    pub(crate) info: Option<VmInfo>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct VmInfo {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) boot_error: String,
    #[serde(default)]
    pub(crate) interfaces: Vec<NetworkInterfaceStatus>,
    #[serde(default)]
    pub(crate) running: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct NetworkInterfaceStatus {
    #[serde(default)]
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) mac: String,
}

#[derive(Clone)]
pub(crate) struct VmmClient {
    ssh: Ssh,
    ssh_options: SshOptions,
    vmm_home: String,
    label: String,
}

impl VmmClient {
    pub(crate) fn new(
        ssh: Ssh,
        ssh_options: SshOptions,
        vmm_home: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            ssh,
            ssh_options,
            vmm_home: vmm_home.into(),
            label: label.into(),
        }
    }

    pub(crate) fn create_vm(&self, request: &CreateVmRequest) -> Result<String> {
        let response: IdMessage = self.prpc_call("CreateVm", request)?;
        validate_vm_id(&response.id)?;
        Ok(response.id)
    }

    pub(crate) fn start_vm(&self, vm_id: &str) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StartVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
        )?;
        Ok(())
    }

    pub(crate) fn stop_vm(&self, vm_id: &str) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "StopVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
        )?;
        Ok(())
    }

    pub(crate) fn remove_vm(&self, vm_id: &str) -> Result<()> {
        validate_vm_id(vm_id)?;
        let _: serde_json::Value = self.prpc_call(
            "RemoveVm",
            &IdMessage {
                id: vm_id.to_owned(),
            },
        )?;
        Ok(())
    }

    pub(crate) fn status(&self, request: &StatusRequest) -> Result<StatusResponse> {
        let response: StatusResponse = self.prpc_call("Status", request)?;
        for vm in &response.vms {
            if !vm.id.is_empty() {
                validate_vm_id(&vm.id)?;
            }
        }
        Ok(response)
    }

    pub(crate) fn get_info(&self, vm_id: &str) -> Result<GetInfoResponse> {
        validate_vm_id(vm_id)?;
        let response: GetInfoResponse = self.prpc_call(
            "GetInfo",
            &IdMessage {
                id: vm_id.to_owned(),
            },
        )?;
        if let Some(info) = &response.info {
            if !info.id.is_empty() {
                validate_vm_id(&info.id)?;
            }
        }
        Ok(response)
    }

    /// Read the tail of the VM serial log from `<vmm_home>/vm/<vm_id>/serial.log`.
    pub(crate) fn serial_log_tail(&self, vm_id: &str, lines: usize) -> Result<String> {
        let command = build_serial_log_command(&self.vmm_home, vm_id, lines)?;
        self.ssh_options
            .capture_ssh(&self.ssh, &self.label, &command)
    }

    fn prpc_call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: &str,
        request: &Req,
    ) -> Result<Resp> {
        let payload = serde_json::to_vec(request)?;
        let sock_expr = remote_vmm_path(&self.vmm_home, "run/vmm.sock");
        let command = format!(
            "curl -sS --connect-timeout 5 --max-time 60 --unix-socket {sock_expr} -H 'Content-Type: application/json' -w '\\n%{{http_code}}' -X POST --data-binary @- {}",
            shell_quote(&format!("http://localhost/prpc/{method}?json"))
        );
        let raw = self
            .ssh_options
            .capture_ssh_stdin(&self.ssh, &self.label, &command, &payload)
            .wrap_err_with(|| format!("VMM pRPC {method} failed on {}", self.label))?;
        let body = parse_curl_http_response(method, &raw)?;
        if body.trim().is_empty() {
            return serde_json::from_str("{}")
                .wrap_err_with(|| format!("VMM pRPC {method} returned empty body"));
        }
        serde_json::from_str(body)
            .wrap_err_with(|| format!("invalid JSON from VMM pRPC {method}: {}", body.trim()))
    }
}

/// Validate that `vm_id` is a canonical 36-character UUID (`8-4-4-4-12` hex).
pub(crate) fn validate_vm_id(vm_id: &str) -> Result<()> {
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

/// Build the remote shell command to read the serial log tail safely.
pub(crate) fn build_serial_log_command(
    vmm_home: &str,
    vm_id: &str,
    lines: usize,
) -> Result<String> {
    validate_vm_id(vm_id)?;
    let log_expr = remote_vmm_path(vmm_home, &format!("vm/{vm_id}/serial.log"));
    let sock_expr = remote_vmm_path(vmm_home, "run/vmm.sock");
    let encoded_id = percent_encode_query_component(vm_id);
    let url =
        format!("http://localhost/logs?id={encoded_id}&follow=false&ansi=false&lines={lines}");
    let url_quoted = shell_quote(&url);
    Ok(format!(
        "if [ -f {log_expr} ]; then tail -n {lines} {log_expr}; else curl -fsS --unix-socket {sock_expr} {url_quoted} 2>/dev/null || true; fi"
    ))
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Build a shell expression for `<vmm_home>/<relative>`, expanding a leading `~/`.
pub(crate) fn remote_vmm_path(vmm_home: &str, relative: &str) -> String {
    let trimmed_home = vmm_home.trim_end_matches('/');
    let trimmed_rel = relative.trim_start_matches('/');
    let joined = if trimmed_rel.is_empty() {
        trimmed_home.to_owned()
    } else {
        format!("{trimmed_home}/{trimmed_rel}")
    };
    if let Some(rest) = joined.strip_prefix("~/") {
        format!("\"$HOME\"/{}", shell_quote(rest))
    } else if joined == "~" {
        "\"$HOME\"".to_owned()
    } else {
        shell_quote(&joined)
    }
}

/// Compute the dstack `app_id` (first 40 lowercase hex chars of SHA-256 of `compose_file`).
pub(crate) fn calc_app_id(compose_file: &str) -> String {
    let digest = Sha256::digest(compose_file.as_bytes());
    hex::encode(digest)[..40].to_owned()
}

/// Extract the primary NIC MAC address from VMM `VmInfo`.
pub(crate) fn extract_primary_mac(info: &VmInfo) -> Result<String> {
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
pub(crate) fn normalize_mac(raw: &str) -> Result<String> {
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

fn parse_curl_http_response<'a>(method: &str, raw: &'a str) -> Result<&'a str> {
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    let (body, status_str) = match trimmed.rsplit_once('\n') {
        Some((body, code)) => (body, code.trim()),
        None => ("", trimmed.trim()),
    };
    let status: u16 = status_str
        .parse()
        .wrap_err_with(|| format!("VMM pRPC {method} returned non-HTTP output: {trimmed}"))?;
    if !(200..300).contains(&status) {
        bail!("VMM pRPC {method} returned HTTP {status}: {}", body.trim());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn create_vm_request_matches_dstack_prpc_shape() {
        let app_id = calc_app_id(APP_COMPOSE_JSON);
        assert_eq!(app_id.len(), 40);
        assert!(app_id.bytes().all(|b| b.is_ascii_hexdigit()));

        let req = CreateVmRequest {
            name: "vm-devnet-validator-0".into(),
            image: "outbe-dev-0.1".into(),
            compose_file: APP_COMPOSE_JSON.into(),
            vcpu: 3,
            memory: 5 * 1024,
            disk_size: 40,
            ports: Vec::new(),
            app_id: Some(app_id.clone()),
            user_config: String::new(),
            hugepages: false,
            pin_numa: false,
            stopped: true,
            no_tee: false,
            networking: Some(NetworkingConfig {
                mode: "bridge".into(),
                bridge_name: "obr0".into(),
            }),
            annotations: BTreeMap::new(),
        };
        let value: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["name"], json!("vm-devnet-validator-0"));
        assert_eq!(value["image"], json!("outbe-dev-0.1"));
        assert_eq!(value["vcpu"], json!(3));
        assert_eq!(value["memory"], json!(5120));
        assert_eq!(value["disk_size"], json!(40));
        assert_eq!(value["ports"], json!([]));
        assert_eq!(value["app_id"], json!(app_id));
        assert_eq!(value["user_config"], json!(""));
        assert_eq!(value["hugepages"], json!(false));
        assert_eq!(value["pin_numa"], json!(false));
        assert_eq!(value["stopped"], json!(true));
        assert_eq!(value["no_tee"], json!(false));
        assert_eq!(
            value["networking"],
            json!({"mode": "bridge", "bridge_name": "obr0"})
        );
        assert_eq!(value["annotations"], json!({}));
        // Ensure KMS/gateway/encrypted_env fields are absent for key_provider: none.
        assert!(value.get("kms_urls").is_none());
        assert!(value.get("gateway_urls").is_none());
        assert!(value.get("encrypted_env").is_none());
    }

    #[test]
    fn id_and_status_requests_match_dstack_prpc_shape() {
        let id_req = serde_json::to_value(&IdMessage {
            id: "abc-123".into(),
        })
        .unwrap();
        assert_eq!(id_req, json!({"id": "abc-123"}));

        let status_req = serde_json::to_value(&StatusRequest {
            ids: vec!["vm-1".into()],
            brief: true,
            ..StatusRequest::default()
        })
        .unwrap();
        assert_eq!(
            status_req,
            json!({
                "ids": ["vm-1"],
                "brief": true,
                "keyword": "",
                "page": 0,
                "page_size": 0,
                "status": ""
            })
        );
    }

    #[test]
    fn extract_primary_mac_prefers_bridge_and_rejects_invalid() {
        let response: GetInfoResponse = serde_json::from_value(json!({
            "found": true,
            "info": {
                "id": "vm-42",
                "name": "validator-0",
                "status": "stopped",
                "running": false,
                "interfaces": [
                    { "mode": "user", "backend": "slirp", "mac": "52:54:00:11:22:33" },
                    { "mode": "bridge", "backend": "tap_bridge", "mac": "52:54:00:AA:BB:CC", "bridge_name": "obr0" }
                ]
            }
        }))
        .unwrap();
        let info = response.info.unwrap();
        assert_eq!(extract_primary_mac(&info).unwrap(), "52:54:00:aa:bb:cc");

        let empty = VmInfo::default();
        assert!(extract_primary_mac(&empty).is_err());

        assert!(normalize_mac("not-a-mac").is_err());
        assert!(normalize_mac("52:54:00:11:22").is_err());
    }

    #[test]
    fn remote_vmm_path_expands_tilde_safely() {
        assert_eq!(
            remote_vmm_path("~/.dstack-vmm", "run/vmm.sock"),
            "\"$HOME\"/'.dstack-vmm/run/vmm.sock'"
        );
        assert_eq!(
            remote_vmm_path("/srv/dstack-vmm/", "vm/abc/hda.img"),
            "'/srv/dstack-vmm/vm/abc/hda.img'"
        );
    }

    #[test]
    fn parse_curl_http_response_handles_ok_empty_and_errors() {
        assert_eq!(
            parse_curl_http_response("CreateVm", "{\"id\":\"vm-1\"}\n200\n").unwrap(),
            "{\"id\":\"vm-1\"}"
        );
        assert_eq!(parse_curl_http_response("StartVm", "200\n").unwrap(), "");
        let err = parse_curl_http_response("CreateVm", "already exists\n400\n").unwrap_err();
        assert!(err.to_string().contains("HTTP 400"), "{err}");
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn validate_vm_id_rejects_shell_injection_and_non_uuid() {
        let valid = "550e8400-e29b-41d4-a716-446655440000";
        validate_vm_id(valid).unwrap();
        let cmd = build_serial_log_command("~/.dstack-vmm", valid, 40).unwrap();
        assert!(cmd.contains(&format!(
            "'http://localhost/logs?id={valid}&follow=false&ansi=false&lines=40'"
        )));

        let injection = "x'; printf REVIEW_INJECTION_OK >&2; fi; #";
        assert!(validate_vm_id(injection).is_err());
        assert!(build_serial_log_command("~/.dstack-vmm", injection, 40).is_err());
        assert!(validate_vm_id("vm-0001").is_err());
        assert!(validate_vm_id("").is_err());
    }
}
