//! `outbe-up host status` and `outbe-up host setup`: inspect the local host and report the
//! missing VM networking components.

use std::path::Path;

use eyre::{ensure, Result};

use super::{
    bridge::{detect_bridge, detect_default_uplink},
    dhcp::detect_dhcp,
    firewall::{classify_egress_status, detect_firewall_and_egress},
    platform::detect_platform,
    sysctl::{probe_managed_nat_files, read_runtime_ip_forward, scan_persistent_ip_forward},
    vmm_status::detect_vmm_status,
    HostEnv, HostSetupReport, HostStatusReport, NatStatus, VmmStatus, DEFAULT_BRIDGE_NAME,
    HOST_STATUS_SCHEMA_VERSION,
};
use crate::{
    nat::validate_iface_name,
    vmm::{VmmClient, VmmTransport},
};

/// Inspect the local host and return a stable v1 [`HostStatusReport`]. Strictly read-only.
pub fn inspect_host_status<T: VmmTransport>(
    vmm: &VmmClient<T>,
    vmm_socket: &Path,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostStatusReport> {
    let mut notes = Vec::new();
    let vmm_status = detect_vmm_status(vmm, vmm_socket);
    if !vmm_status.reachable {
        notes.push(format!(
            "local VMM socket {} is not reachable",
            vmm_socket.display()
        ));
    }

    let bridge_name = resolve_status_bridge(bridge_override, &vmm_status)?;

    let (bridge, bridge_subnet) = detect_bridge(&bridge_name);
    let dhcp = detect_dhcp(&bridge_name, env);
    let uplink = detect_default_uplink();
    let ip_forward_runtime = read_runtime_ip_forward(env);
    let ip_forward_persistent = scan_persistent_ip_forward(&env.nat_paths);
    let managed_files = probe_managed_nat_files(&env.nat_paths);

    let (firewall, existing_egress, partial_egress) =
        detect_firewall_and_egress(&bridge_name, bridge_subnet, uplink.as_deref());
    if !firewall.nft_accessible {
        notes.push(
            "nftables ruleset unavailable (requires CAP_NET_ADMIN / passwordless `sudo -n nft`)"
                .to_owned(),
        );
    } else if !firewall.unparsed_dnat_rules.is_empty() {
        notes.push(format!(
            "live nftables ruleset contains {} unparseable DNAT rule(s); `outbe-up host publish` will fail closed until resolved",
            firewall.unparsed_dnat_rules.len()
        ));
    }

    let egress_status = classify_egress_status(
        firewall.nft_accessible,
        firewall.managed_nat_table_present,
        &existing_egress,
        &partial_egress,
    );

    let nat = NatStatus {
        uplink,
        ip_forward_runtime,
        ip_forward_persistent,
        egress_status: egress_status.to_owned(),
        existing_egress,
        partial_egress,
        managed_files,
    };

    let platform = detect_platform(env);
    if platform.qgs.required && !platform.qgs.unit_active {
        notes.push("Intel TDX detected but qgsd.service is not active".to_owned());
    }

    let ready_for_vm =
        bridge.is_ready() && dhcp.is_serving() && vmm_status.reachable && nat.has_egress();

    Ok(HostStatusReport {
        version: HOST_STATUS_SCHEMA_VERSION,
        bridge,
        dhcp,
        firewall,
        nat,
        vmm: vmm_status,
        platform,
        ready_for_vm,
        notes,
    })
}

/// Evaluate `outbe-up host setup`: pure read-only no-op when bridge + DHCP + egress are already
/// configured; on an unconfigured host prints exact steps and refuses to write without owner
/// design approval.
pub fn evaluate_host_setup<T: VmmTransport>(
    vmm: &VmmClient<T>,
    vmm_socket: &Path,
    bridge_override: Option<&str>,
    dry_run: bool,
    env: &HostEnv,
) -> Result<HostSetupReport> {
    let status = inspect_host_status(vmm, vmm_socket, bridge_override, env)?;
    ensure!(
        status.firewall.nft_accessible,
        "cannot verify host firewall/egress state: `nft -j list ruleset` is inaccessible (requires root or passwordless `sudo -n nft`)"
    );

    let mut missing_components = Vec::new();
    let mut planned_steps = Vec::new();

    if !status.bridge.is_ready() {
        missing_components.push(format!("bridge `{}`", status.bridge.name));
        planned_steps.push(format!(
            "Create and persist VM bridge `{}` (e.g. `10.77.0.1/24`) via systemd unit `vm-bridge.service`.",
            status.bridge.name
        ));
    }
    if !status.dhcp.is_serving() {
        missing_components.push(format!("DHCP service on bridge `{}`", status.bridge.name));
        planned_steps.push(format!(
            "Configure and enable a dnsmasq DHCP service on `{}` with `--dhcp-range` and `--dhcp-leasefile`.",
            status.bridge.name
        ));
    }
    if !status.nat.has_egress() {
        missing_components.push("outbound NAT / IPv4 forwarding".to_owned());
        planned_steps.push(format!(
            "Enable persistent `net.ipv4.ip_forward=1` and outbound masquerade for `{}` (e.g. `outbe-up host nat --bridge {}`).",
            status.bridge.name, status.bridge.name
        ));
    }

    let already_configured = missing_components.is_empty();
    let approval_note = if already_configured {
        None
    } else {
        Some(
            "Refusing to provision missing host components automatically: owner design approval is required before outbe-up creates host bridge/DHCP services (zero changes made)."
                .to_owned(),
        )
    };

    Ok(HostSetupReport {
        already_configured,
        dry_run,
        writes_performed: 0,
        missing_components,
        planned_steps,
        approval_note,
        status,
    })
}

/// Use `--bridge` when given. Otherwise use the VMM default bridge when it is a valid interface
/// name, or else [`DEFAULT_BRIDGE_NAME`].
fn resolve_status_bridge(bridge_override: Option<&str>, vmm_status: &VmmStatus) -> Result<String> {
    if let Some(name) = bridge_override {
        validate_iface_name(name, "bridge")?;
        return Ok(name.to_owned());
    }
    Ok(vmm_status
        .default_bridge
        .as_deref()
        .filter(|b| !b.is_empty() && validate_iface_name(b, "bridge").is_ok())
        .unwrap_or(DEFAULT_BRIDGE_NAME)
        .to_owned())
}
