//! VM bridge and default uplink detection from `ip` and `networkctl` output.

use std::net::Ipv4Addr;

use serde_json::Value;

use super::{
    probe::{query_systemd_unit, run_capture, SystemdUnitProps},
    BridgeStatus,
};
use crate::nat::{validate_iface_name, Ipv4Subnet};

const VM_BRIDGE_SERVICE: &str = "vm-bridge.service";

pub(super) fn detect_bridge(bridge_name: &str) -> (BridgeStatus, Option<Ipv4Subnet>) {
    let unit = query_systemd_unit(VM_BRIDGE_SERVICE);
    let raw_ip = run_capture("ip", &["-j", "addr", "show", "dev", bridge_name]);
    let parsed = raw_ip.as_deref().and_then(parse_ip_json_bridge_addr);

    let Some((up, gateway_ip, subnet)) = parsed else {
        return (
            BridgeStatus {
                name: bridge_name.to_owned(),
                present: false,
                up: false,
                cidr: None,
                gateway_ip: None,
                management: "absent".to_owned(),
                unit_name: unit.is_loaded().then(|| VM_BRIDGE_SERVICE.to_owned()),
                unit_enabled: unit.is_enabled(),
                unit_active: unit.is_active(),
            },
            None,
        );
    };

    (
        BridgeStatus {
            name: bridge_name.to_owned(),
            present: true,
            up,
            cidr: subnet.map(Ipv4Subnet::cidr_string),
            gateway_ip,
            management: bridge_management(&unit, bridge_name),
            unit_name: unit.is_loaded().then(|| VM_BRIDGE_SERVICE.to_owned()),
            unit_enabled: unit.is_enabled(),
            unit_active: unit.is_active(),
        },
        subnet,
    )
}

/// Name the component that manages a present bridge: the bridge systemd unit,
/// systemd-networkd, or manual setup.
fn bridge_management(unit: &SystemdUnitProps, bridge_name: &str) -> String {
    let unit_manages_bridge =
        unit.is_loaded() && (unit.is_active() || unit.exec_start.contains(bridge_name));
    if unit_manages_bridge {
        return format!("systemd:{VM_BRIDGE_SERVICE}");
    }
    let networkd_manages_bridge = run_capture("networkctl", &["status", bridge_name, "--no-pager"])
        .is_some_and(|nctl| {
            nctl.lines()
                .any(|l| l.contains("Network File:") && !l.contains("n/a"))
        });
    if networkd_manages_bridge {
        "systemd-networkd".to_owned()
    } else {
        "manual".to_owned()
    }
}

pub(crate) fn parse_ip_json_bridge_addr(
    json_str: &str,
) -> Option<(bool, Option<Ipv4Addr>, Option<Ipv4Subnet>)> {
    let val: Value = serde_json::from_str(json_str).ok()?;
    let iface = val.as_array()?.first()?;
    let operstate = iface.get("operstate").and_then(Value::as_str).unwrap_or("");
    let flags_up = iface
        .get("flags")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|f| f.as_str() == Some("UP")));
    let up = operstate == "UP" || flags_up;

    let first_inet = iface
        .get("addr_info")
        .and_then(Value::as_array)
        .and_then(|addrs| addrs.iter().find_map(inet_local_addr));
    let gateway_ip = first_inet.map(|(ip, _)| ip);
    let subnet = first_inet.and_then(|(ip, prefix)| Ipv4Subnet::from_addr_prefix(ip, prefix).ok());
    Some((up, gateway_ip, subnet))
}

/// Return the local IPv4 address and prefix length of one `inet` entry of `addr_info`.
fn inet_local_addr(addr: &Value) -> Option<(Ipv4Addr, u8)> {
    if addr.get("family").and_then(Value::as_str) != Some("inet") {
        return None;
    }
    let ip = addr.get("local")?.as_str()?.parse::<Ipv4Addr>().ok()?;
    let prefix = u8::try_from(addr.get("prefixlen")?.as_u64()?).ok()?;
    Some((ip, prefix))
}

pub(super) fn detect_default_uplink() -> Option<String> {
    let raw = run_capture("ip", &["-4", "-o", "route", "show", "default"])?;
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(route_uplink)
}

/// Return the valid interface name after `dev` in one `ip route` line.
fn route_uplink(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let pos = tokens.iter().position(|&t| t == "dev")?;
    let iface = *tokens.get(pos + 1)?;
    validate_iface_name(iface, "uplink")
        .is_ok()
        .then(|| iface.to_owned())
}
