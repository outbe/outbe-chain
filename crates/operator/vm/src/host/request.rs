//! Operator input parsing shared by `outbe-up host check` and `outbe-up host publish`: port
//! specifications, source CIDRs, and the recorded local VM matched against the live VMM.

use std::{collections::BTreeSet, fs, net::Ipv4Addr};

use eyre::{bail, ensure, Context, Result};

use super::{PublishPortSpec, TransportProto};
use crate::{
    local::{LocalVmPhase, LocalVmState, LocalVmStore},
    vmm::{VmInfo, VmmClient, VmmTransport},
};

/// Parse all `--port` values and reject a repeated `(proto, host port)` pair.
pub(super) fn parse_port_specs(raw_ports: &[String]) -> Result<Vec<PublishPortSpec>> {
    let mut ports = Vec::with_capacity(raw_ports.len());
    let mut seen_host_ports = BTreeSet::new();
    for raw in raw_ports {
        let spec = parse_port_spec(raw)?;
        ensure!(
            seen_host_ports.insert((spec.proto.as_str(), spec.host_port)),
            "duplicate host port {}/{} in --port arguments",
            spec.proto.as_str(),
            spec.host_port
        );
        ports.push(spec);
    }
    Ok(ports)
}

/// Normalize all `--source` values. No values give an empty list.
pub(super) fn normalize_sources(raw_sources: &[String]) -> Result<Vec<String>> {
    raw_sources.iter().map(|s| normalize_ipv4_cidr(s)).collect()
}

/// Return the given sources, or `0.0.0.0/0` when there are none.
pub(super) fn sources_or_any(sources: &[String]) -> Vec<String> {
    if sources.is_empty() {
        vec!["0.0.0.0/0".to_owned()]
    } else {
        sources.to_vec()
    }
}

/// Load the recorded state of `vm_name` and the matching live VMM info.
///
/// `action` completes the error message for a VM that cannot be used ("checked" or "published").
pub(super) fn load_live_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    vm_name: &str,
    action: &str,
) -> Result<(LocalVmState, VmInfo)> {
    let state_path = store.state_path(vm_name)?;
    let raw_state = fs::read_to_string(&state_path).wrap_err_with(|| {
        format!(
            "local VM `{vm_name}` is not recorded in {} (create it first with `outbe-up vm create`)",
            state_path.display()
        )
    })?;
    let state: LocalVmState = serde_json::from_str(&raw_state)
        .wrap_err_with(|| format!("parse local VM state {}", state_path.display()))?;
    ensure!(
        !state.vm_id.is_empty() && state.phase != LocalVmPhase::Removing,
        "VM `{vm_name}` is in phase `{:?}` and cannot be {action}",
        state.phase
    );

    let info_resp = vmm.get_info(&state.vm_id)?;
    ensure!(
        !info_resp.is_gone(),
        "VM `{vm_name}` ({}) is missing from local VMM",
        state.vm_id
    );
    let info = info_resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    Ok((state, info))
}

/// Parse `<guest>[:<host>][/(tcp|udp)]` (defaults to `host == guest` and `proto == tcp`).
pub fn parse_port_spec(raw: &str) -> Result<PublishPortSpec> {
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "port specification must not be empty");
    let (ports_part, proto) = match trimmed.split_once('/') {
        Some((p, proto_str)) => {
            let proto = match proto_str.to_ascii_lowercase().as_str() {
                "tcp" => TransportProto::Tcp,
                "udp" => TransportProto::Udp,
                other => {
                    bail!("unsupported protocol '{other}' in --port '{raw}' (expected tcp or udp)");
                }
            };
            (p, proto)
        }
        None => (trimmed, TransportProto::Tcp),
    };
    let (guest_port, host_port) = match ports_part.split_once(':') {
        Some((g, h)) => {
            let guest: u16 = g
                .parse()
                .wrap_err_with(|| format!("invalid guest port in --port '{raw}'"))?;
            let host: u16 = h
                .parse()
                .wrap_err_with(|| format!("invalid host port in --port '{raw}'"))?;
            (guest, host)
        }
        None => {
            let port: u16 = ports_part
                .parse()
                .wrap_err_with(|| format!("invalid port in --port '{raw}'"))?;
            (port, port)
        }
    };
    ensure!(
        guest_port > 0 && host_port > 0,
        "ports must be between 1 and 65535 in --port '{raw}'"
    );
    Ok(PublishPortSpec {
        guest_port,
        host_port,
        proto,
    })
}

/// Normalize an IPv4 address or CIDR string (`1.2.3.4` -> `1.2.3.4/32`, `10.77.0.0/24` -> `10.77.0.0/24`).
pub fn normalize_ipv4_cidr(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if let Some((ip_str, prefix_str)) = trimmed.split_once('/') {
        let addr: Ipv4Addr = ip_str
            .parse()
            .wrap_err_with(|| format!("invalid IPv4 address in --source '{raw}'"))?;
        let prefix: u8 = prefix_str
            .parse()
            .wrap_err_with(|| format!("invalid CIDR prefix in --source '{raw}'"))?;
        ensure!(
            prefix <= 32,
            "CIDR prefix must be 0..=32 in --source '{raw}'"
        );
        let mask = if prefix == 0 {
            0u32
        } else {
            !0u32 << (32 - prefix)
        };
        let network = Ipv4Addr::from(u32::from(addr) & mask);
        Ok(format!("{network}/{prefix}"))
    } else {
        let addr: Ipv4Addr = trimmed
            .parse()
            .wrap_err_with(|| format!("invalid IPv4 address/CIDR in --source '{raw}'"))?;
        Ok(format!("{addr}/32"))
    }
}
