//! `outbe-up host publish`: render `/etc/host-firewall.nft` DNAT and forward rules for one VM.

use std::{fs, net::Ipv4Addr, path::PathBuf};

use eyre::{bail, ensure, Result};

use super::{
    bridge::{detect_bridge, detect_default_uplink},
    dhcp::detect_dhcp,
    firewall::detect_firewall_and_egress,
    port_rules::{
        describe_unparsed_dnat_rules, dnat_relation, resolve_public_ip, DnatRelation,
        FirewallTargets, PortRuleRenderer, HOST_FIREWALL_NFT_PATH, HOST_FIREWALL_SERVICE,
    },
    request::{load_live_vm, normalize_sources, parse_port_specs, sources_or_any},
    DhcpStatus, FirewallDnatRule, HostEnv, HostPublishReport, PublishPortConflict, PublishPortSpec,
    VmPortsRequest,
};
use crate::{
    dnsmasq::lookup_lease_ip_by_mac,
    local::{validate_vm_name, LocalVmState, LocalVmStore},
    nat::validate_iface_name,
    vmm::{extract_primary_mac, VmmClient, VmmTransport},
};

/// Compute and render `/etc/host-firewall.nft` DNAT + forward-accept rules for one local VM,
/// detecting conflicts against the live nftables ruleset. Strictly read-only (never writes).
pub fn plan_host_publish<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    request: &VmPortsRequest<'_>,
    env: &HostEnv,
) -> Result<HostPublishReport> {
    let vm_name = request.vm_name;
    validate_vm_name(vm_name)?;
    ensure!(
        !request.raw_ports.is_empty(),
        "at least one `--port <guest>[:<host>][/(tcp|udp)]` specification is required"
    );
    let ports = parse_port_specs(request.raw_ports)?;
    let sources = sources_or_any(&normalize_sources(request.raw_sources)?);

    let (state, info) = load_live_vm(store, vmm, vm_name, "published")?;
    let live_mac = extract_primary_mac(&info).unwrap_or_else(|_| state.mac.clone());

    let bridge_name = request.bridge.unwrap_or(&state.bridge).to_owned();
    validate_iface_name(&bridge_name, "bridge")?;
    let (_, bridge_subnet) = detect_bridge(&bridge_name);
    let dhcp = detect_dhcp(&bridge_name, env);
    let guest_ip = resolve_guest_ip(&state, &dhcp, vm_name, &live_mac)?;

    let uplink = detect_default_uplink();
    let (firewall, _, _) =
        detect_firewall_and_egress(&bridge_name, bridge_subnet, uplink.as_deref());
    ensure!(
        firewall.nft_accessible,
        "cannot inspect live firewall rules: `nft -j list ruleset` is inaccessible (requires root or passwordless `sudo -n nft`)"
    );
    if !firewall.unparsed_dnat_rules.is_empty() {
        bail!(
            "refusing to plan port publish: live nftables ruleset contains {} unparseable DNAT rule(s) so port conflicts cannot be verified safely: {}",
            firewall.unparsed_dnat_rules.len(),
            describe_unparsed_dnat_rules(&firewall.unparsed_dnat_rules)
        );
    }

    let public_ip = resolve_public_ip(request.public_ip, &firewall.dnat_rules, uplink.as_deref())?;
    let (conflicts, already_published_ports) =
        compare_with_live_dnat(&ports, &firewall.dnat_rules, public_ip, guest_ip);

    let targets = FirewallTargets::detect(&firewall);
    let renderer = PortRuleRenderer::new(vm_name, &sources, guest_ip, public_ip);
    let forward_rules = ports.iter().map(|s| renderer.forward_rule(s)).collect();
    let prerouting_rules = ports.iter().map(|s| renderer.prerouting_rule(s)).collect();
    let apply_commands = targets.apply_commands();
    let remove_commands = vec![
        format!(
            "Remove the lines tagged `comment \"{}\"` from `{HOST_FIREWALL_NFT_PATH}`",
            renderer.comment()
        ),
        format!("sudo nft --check --file {HOST_FIREWALL_NFT_PATH}"),
        format!("sudo systemctl reload {HOST_FIREWALL_SERVICE}"),
    ];

    Ok(HostPublishReport {
        vm: vm_name.to_owned(),
        vm_id: state.vm_id,
        guest_ip,
        public_ip,
        sources,
        ports,
        firewall_file: PathBuf::from(HOST_FIREWALL_NFT_PATH),
        firewall_service: HOST_FIREWALL_SERVICE.to_owned(),
        forward_table: targets.forward_table,
        forward_policy: targets.forward_policy,
        nat_table: targets.nat_table,
        forward_rules,
        prerouting_rules,
        apply_commands,
        remove_commands,
        conflicts,
        already_published_ports,
        writes_performed: 0,
    })
}

/// Use the recorded intended IP of the VM, or else its active DHCP lease.
fn resolve_guest_ip(
    state: &LocalVmState,
    dhcp: &DhcpStatus,
    vm_name: &str,
    live_mac: &str,
) -> Result<Ipv4Addr> {
    if let Some(ip) = state.intended_ip {
        return Ok(ip);
    }
    let Some(leases_file) = &dhcp.leases_file else {
        bail!(
            "VM `{vm_name}` has no recorded intended_ip and no DHCP lease file was detected; recreate the VM with `--ip <ipv4>`"
        );
    };
    let content = fs::read_to_string(leases_file).unwrap_or_default();
    lookup_lease_ip_by_mac(&content, live_mac).ok_or_else(|| {
        eyre::eyre!(
            "VM `{vm_name}` has no recorded intended_ip and no active DHCP lease for MAC `{live_mac}`; recreate the VM with `--ip <ipv4>`"
        )
    })
}

/// Compare each requested port with the live DNAT rules. Return the conflicts and the ports
/// that a live rule already publishes to the same target.
fn compare_with_live_dnat(
    ports: &[PublishPortSpec],
    dnat_rules: &[FirewallDnatRule],
    public_ip: Ipv4Addr,
    guest_ip: Ipv4Addr,
) -> (Vec<PublishPortConflict>, Vec<PublishPortSpec>) {
    let mut conflicts = Vec::new();
    let mut already_published_ports = Vec::new();
    for spec in ports {
        for rule in dnat_rules {
            match dnat_relation(rule, spec, public_ip, guest_ip) {
                DnatRelation::Unrelated => {}
                DnatRelation::SameTarget => already_published_ports.push(*spec),
                DnatRelation::Conflict => {
                    conflicts.push(PublishPortConflict::from_rule(rule, spec, guest_ip));
                }
            }
        }
    }
    (conflicts, already_published_ports)
}
