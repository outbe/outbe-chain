//! `outbe-up host check`: verify host networking, the static service IP, and published ports of
//! one local VM.

use std::{fs, net::Ipv4Addr};

use eyre::{bail, ensure, Result};

use super::{
    bridge::{detect_bridge, detect_default_uplink},
    dhcp::{detect_dhcp, read_and_validate_hosts_file},
    egress_status_provides_egress,
    firewall::{classify_egress_status, detect_firewall_and_egress},
    request::{load_live_vm, normalize_sources, parse_port_specs},
    sysctl::read_runtime_ip_forward,
    DhcpStatus, FirewallStatus, HostCheckReport, HostEnv, PublishPortSpec, VmPortsRequest,
};
use crate::{
    dnsmasq::{check_dnsmasq_lease_conflicts, check_dnsmasq_reservation_status},
    local::{validate_vm_name, LocalVmState, LocalVmStore},
    nat::{validate_iface_name, Ipv4Subnet},
    vmm::{extract_primary_mac, VmmClient, VmmTransport},
};

mod ports;

/// The VM and the published ports under check.
struct CheckSubject<'a> {
    vm_name: &'a str,
    ip: Ipv4Addr,
    live_mac: &'a str,
    bridge_name: &'a str,
    ports: &'a [PublishPortSpec],
    explicit_sources: &'a [String],
    public_ip_override: Option<Ipv4Addr>,
}

/// Missing prerequisites and conflicts, in the order that the checks find them.
#[derive(Default)]
struct Findings {
    missing: Vec<String>,
    conflicts: Vec<String>,
}

impl Findings {
    fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.conflicts.is_empty()
    }
}

/// Result of the host networking checks.
struct HostNetwork {
    ok: bool,
    bridge_ip_ok: bool,
    dhcp: DhcpStatus,
    firewall: FirewallStatus,
    uplink: Option<String>,
}

/// Strictly read-only verifier checking that host networking (bridge, IPv4 forwarding, outbound
/// NAT), the VM's static internal IP (inside the bridge subnet, outside any host DHCP pool, and
/// unused), and any requested published ports (both prerouting DNAT and forward accept rules in
/// live nftables) are configured with zero conflicts.
///
/// DHCP is not required on the host for static VMs (Decision 10, revised); if a host DHCP service
/// is present, pool exclusion and reservation/lease conflict checks are still enforced.
pub fn check_local_vm_host<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    request: &VmPortsRequest<'_>,
    explicit_ip: Option<Ipv4Addr>,
    env: &HostEnv,
) -> Result<HostCheckReport> {
    let vm_name = request.vm_name;
    validate_vm_name(vm_name)?;
    let ports = parse_port_specs(request.raw_ports)?;
    let explicit_sources = normalize_sources(request.raw_sources)?;

    let (state, info) = load_live_vm(store, vmm, vm_name, "checked")?;
    let live_mac = extract_primary_mac(&info)?;
    if !state.mac.is_empty() {
        ensure!(
            state.mac.eq_ignore_ascii_case(&live_mac),
            "VM `{vm_name}` MAC mismatch: recorded state has `{}`, live VMM reports `{live_mac}`",
            state.mac
        );
    }
    let ip = resolve_service_ip(explicit_ip, &state, vm_name)?;
    let bridge_name = request.bridge.unwrap_or(&state.bridge).to_owned();
    validate_iface_name(&bridge_name, "bridge")?;

    let subject = CheckSubject {
        vm_name,
        ip,
        live_mac: &live_mac,
        bridge_name: &bridge_name,
        ports: &ports,
        explicit_sources: &explicit_sources,
        public_ip_override: request.public_ip,
    };
    let mut findings = Findings::default();

    // 1. Host networking checks (bridge, active DHCP, IPv4 forwarding, outbound NAT egress).
    let network = check_host_network(&subject, env, &mut findings);
    // 2. Static internal IP safety checks (Decision 10, revised):
    //    - Must be inside the bridge subnet (not network, broadcast, or gateway).
    //    - Must be outside any host DHCP pool (`pool_start..=pool_end`) when DHCP is present.
    //    - Must be unused: not assigned to another local VM, any existing reservation in
    //      `dhcp-hosts`, or any active DHCP lease for another MAC.
    let service_ip = check_service_ip(
        &subject,
        store,
        &network.dhcp,
        env.expected_hosts_owner,
        &mut findings,
    )?;
    let service_ip_ok = network.bridge_ip_ok && service_ip.clear;
    // 3. Optional published-port verification (DNAT + forward accept, fail closed on unparsed DNAT).
    let ports_check = ports::check_published_ports(
        &subject,
        &network.firewall,
        network.uplink.as_deref(),
        &mut findings,
    );

    let checks_pass = network.ok && service_ip_ok && ports_check.ok;
    let ready = checks_pass && findings.is_empty();

    Ok(HostCheckReport {
        vm: vm_name.to_owned(),
        vm_id: state.vm_id,
        mac: live_mac,
        ip,
        bridge: bridge_name,
        public_ip: ports_check.public_ip,
        ports,
        ready,
        host_network_ok: network.ok,
        service_ip_ok,
        dhcp_reserved: service_ip.dhcp_reserved,
        published_ports_ok: ports_check.ok,
        missing: findings.missing,
        conflicts: findings.conflicts,
        port_conflicts: ports_check.port_conflicts,
        hosts_file: network.dhcp.hosts_file,
        dhcp_unit: network.dhcp.unit_name,
        recommended_dhcp_block: None,
        recommended_dhcp_reload: None,
        recommended_forward_rules: ports_check.forward_rules,
        recommended_prerouting_rules: ports_check.prerouting_rules,
        recommended_firewall_commands: ports_check.firewall_commands,
        writes_performed: 0,
    })
}

/// Use `--ip` or the recorded intended IP. Both must agree when both are present.
fn resolve_service_ip(
    explicit_ip: Option<Ipv4Addr>,
    state: &LocalVmState,
    vm_name: &str,
) -> Result<Ipv4Addr> {
    match (explicit_ip, state.intended_ip) {
        (Some(given), Some(recorded)) => {
            ensure!(
                given == recorded,
                "VM `{vm_name}` already has recorded intended_ip {recorded}, which disagrees with --ip {given}"
            );
            Ok(given)
        }
        (Some(ip), None) | (None, Some(ip)) => Ok(ip),
        (None, None) => {
            bail!(
                "no IPv4 address for VM `{vm_name}`: pass `--ip <ipv4>` or record one during `outbe-up vm create --ip <ipv4>`"
            );
        }
    }
}

fn check_host_network(
    subject: &CheckSubject<'_>,
    env: &HostEnv,
    findings: &mut Findings,
) -> HostNetwork {
    let bridge_name = subject.bridge_name;
    let (bridge_status, bridge_subnet) = detect_bridge(bridge_name);
    let bridge_ok = bridge_status.is_ready();
    let mut bridge_ip_ok = false;
    if !bridge_ok {
        findings.missing.push(format!(
            "bridge `{bridge_name}` is missing, down, or has no IPv4 CIDR configured"
        ));
    } else if let Some(subnet) = bridge_subnet {
        match bridge_ip_conflict(subject, subnet, bridge_status.gateway_ip) {
            Some(conflict) => findings.conflicts.push(conflict),
            None => bridge_ip_ok = true,
        }
    }

    let dhcp = detect_dhcp(bridge_name, env);

    let ip_forward_runtime = read_runtime_ip_forward(env);
    let ip_forward_ok = ip_forward_runtime == Some(1);
    if !ip_forward_ok {
        findings.missing.push(format!(
            "net.ipv4.ip_forward is not enabled (runtime value: {})",
            ip_forward_runtime.map_or_else(|| "unknown".to_owned(), |v| v.to_string())
        ));
    }

    let uplink = detect_default_uplink();
    let (firewall, egress_ok) =
        check_egress(bridge_name, bridge_subnet, uplink.as_deref(), findings);

    HostNetwork {
        ok: bridge_ok && ip_forward_ok && egress_ok,
        bridge_ip_ok,
        dhcp,
        firewall,
        uplink,
    }
}

/// Describe why the service IP cannot be used inside the bridge subnet, if it cannot.
fn bridge_ip_conflict(
    subject: &CheckSubject<'_>,
    subnet: Ipv4Subnet,
    gateway_ip: Option<Ipv4Addr>,
) -> Option<String> {
    let (ip, bridge_name) = (subject.ip, subject.bridge_name);
    if !subnet.contains(ip) {
        Some(format!(
            "VM service IP {ip} is outside bridge `{bridge_name}` subnet {}",
            subnet.cidr_string()
        ))
    } else if ip == subnet.network || ip == subnet.broadcast() {
        Some(format!(
            "VM service IP {ip} is the network or broadcast address of {}",
            subnet.cidr_string()
        ))
    } else if gateway_ip == Some(ip) {
        Some(format!(
            "VM service IP {ip} is the bridge `{bridge_name}` gateway address"
        ))
    } else {
        None
    }
}

/// Inspect live nftables and record missing outbound NAT egress. Return the firewall status and
/// whether egress is available.
fn check_egress(
    bridge_name: &str,
    bridge_subnet: Option<Ipv4Subnet>,
    uplink: Option<&str>,
    findings: &mut Findings,
) -> (FirewallStatus, bool) {
    let (firewall, existing_egress, partial_egress) =
        detect_firewall_and_egress(bridge_name, bridge_subnet, uplink);
    let egress_status = classify_egress_status(
        firewall.nft_accessible,
        firewall.managed_nat_table_present,
        &existing_egress,
        &partial_egress,
    );
    let egress_ok = firewall.nft_accessible && egress_status_provides_egress(egress_status);
    if !firewall.nft_accessible {
        findings.missing.push(
            "nftables ruleset is inaccessible (requires root or passwordless `sudo -n nft`)"
                .to_owned(),
        );
    } else if !egress_ok {
        findings.missing.push(format!(
            "outbound NAT egress for bridge `{bridge_name}` is not configured (status={egress_status}); run `outbe-up host nat --bridge {bridge_name}` for recommended rules"
        ));
    }
    (firewall, egress_ok)
}

/// Result of the static service IP checks.
struct ServiceIpCheck {
    /// No DHCP pool, local VM, lease, or reservation conflict exists.
    clear: bool,
    /// `dhcp-hosts` already holds exactly this MAC and IP, with no other conflict.
    dhcp_reserved: bool,
}

fn check_service_ip(
    subject: &CheckSubject<'_>,
    store: &LocalVmStore,
    dhcp: &DhcpStatus,
    expected_hosts_owner: (u32, u32),
    findings: &mut Findings,
) -> Result<ServiceIpCheck> {
    let pool_conflict = dhcp_pool_conflict(subject, dhcp);
    let store_conflicts = local_store_conflicts(subject, store)?;
    let lease_conflict = dhcp_lease_conflict(subject, dhcp);
    let (hosts_conflicts, exact_reservation) =
        check_hosts_reservation(subject, dhcp, expected_hosts_owner);

    let dhcp_reserved = exact_reservation && lease_conflict.is_none() && store_conflicts.is_empty();
    let conflicts: Vec<String> = pool_conflict
        .into_iter()
        .chain(store_conflicts)
        .chain(lease_conflict)
        .chain(hosts_conflicts)
        .collect();
    let clear = conflicts.is_empty();
    findings.conflicts.extend(conflicts);
    Ok(ServiceIpCheck {
        clear,
        dhcp_reserved,
    })
}

fn dhcp_pool_conflict(subject: &CheckSubject<'_>, dhcp: &DhcpStatus) -> Option<String> {
    let (Some(pool_start), Some(pool_end)) = (dhcp.pool_start, dhcp.pool_end) else {
        return None;
    };
    let ip_u32 = u32::from(subject.ip);
    let lo = u32::from(pool_start).min(u32::from(pool_end));
    let hi = u32::from(pool_start).max(u32::from(pool_end));
    let (ip, bridge_name) = (subject.ip, subject.bridge_name);
    (lo..=hi).contains(&ip_u32).then(|| {
        format!(
            "VM service IP {ip} overlaps host DHCP pool {pool_start}..={pool_end} on bridge `{bridge_name}`; choose a service IP outside the DHCP pool"
        )
    })
}

/// Find other local VMs that record the same IP or MAC. Unreadable or unparseable state files
/// are skipped.
fn local_store_conflicts(subject: &CheckSubject<'_>, store: &LocalVmStore) -> Result<Vec<String>> {
    let (ip, live_mac) = (subject.ip, subject.live_mac);
    let mut conflicts = Vec::new();
    for other_name in store.list_vm_names()? {
        if other_name == subject.vm_name {
            continue;
        }
        let Some(other) = read_local_vm_state(store, &other_name)? else {
            continue;
        };
        if other.intended_ip == Some(ip) {
            conflicts.push(format!(
                "IP {ip} is already recorded for local VM `{other_name}`"
            ));
        }
        if other.mac.eq_ignore_ascii_case(live_mac) {
            conflicts.push(format!(
                "MAC {live_mac} is already recorded for local VM `{other_name}`"
            ));
        }
    }
    Ok(conflicts)
}

/// Read the state of another local VM. A missing or unparseable state file gives `None`.
fn read_local_vm_state(store: &LocalVmStore, name: &str) -> Result<Option<LocalVmState>> {
    let Ok(raw) = fs::read_to_string(store.state_path(name)?) else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<LocalVmState>(&raw).ok())
}

fn dhcp_lease_conflict(subject: &CheckSubject<'_>, dhcp: &DhcpStatus) -> Option<String> {
    let leases_content = fs::read_to_string(dhcp.leases_file.as_ref()?).ok()?;
    check_dnsmasq_lease_conflicts(
        &leases_content,
        &[(subject.live_mac.to_owned(), subject.ip)],
    )
    .err()
    .map(|err| err.to_string())
}

/// Compare the VM with `dhcp-hosts`. Return the reservation conflicts and whether an exact
/// reservation for this MAC and IP exists.
fn check_hosts_reservation(
    subject: &CheckSubject<'_>,
    dhcp: &DhcpStatus,
    expected_owner: (u32, u32),
) -> (Vec<String>, bool) {
    let Some(hosts_file) = &dhcp.hosts_file else {
        return (Vec::new(), false);
    };
    let checked = read_and_validate_hosts_file(hosts_file, expected_owner).and_then(|existing| {
        check_dnsmasq_reservation_status(&existing, subject.live_mac, subject.ip)
    });
    match checked {
        Ok(res) if !res.conflicts.is_empty() => (res.conflicts, false),
        Ok(res) => (Vec::new(), res.exact_match),
        Err(err) => (vec![err.to_string()], false),
    }
}
