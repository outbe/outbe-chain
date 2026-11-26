//! Published-port verification for `outbe-up host check`: each requested port needs a matching
//! prerouting DNAT rule and a forward accept rule in live nftables.

use std::net::Ipv4Addr;

use super::{CheckSubject, Findings};
use crate::host::{
    port_rules::{
        describe_unparsed_dnat_rules, dnat_relation, resolve_public_ip, DnatRelation,
        FirewallTargets, PortRuleRenderer,
    },
    request::sources_or_any,
    FirewallDnatRule, FirewallForwardAllow, FirewallStatus, PublishPortConflict, PublishPortSpec,
};

/// Result of the published-port checks.
pub(super) struct PortsCheck {
    pub(super) ok: bool,
    pub(super) public_ip: Option<Ipv4Addr>,
    pub(super) forward_rules: Vec<String>,
    pub(super) prerouting_rules: Vec<String>,
    pub(super) firewall_commands: Vec<String>,
    pub(super) port_conflicts: Vec<PublishPortConflict>,
}

impl PortsCheck {
    fn new(ok: bool, public_ip: Option<Ipv4Addr>) -> Self {
        Self {
            ok,
            public_ip,
            forward_rules: Vec::new(),
            prerouting_rules: Vec::new(),
            firewall_commands: Vec::new(),
            port_conflicts: Vec::new(),
        }
    }
}

pub(super) fn check_published_ports(
    subject: &CheckSubject<'_>,
    firewall: &FirewallStatus,
    uplink: Option<&str>,
    findings: &mut Findings,
) -> PortsCheck {
    let public_ip_override = subject.public_ip_override;
    if subject.ports.is_empty() {
        return PortsCheck::new(true, public_ip_override);
    }
    if !firewall.nft_accessible {
        return PortsCheck::new(false, public_ip_override);
    }
    if !firewall.unparsed_dnat_rules.is_empty() {
        findings.conflicts.push(format!(
            "live nftables ruleset contains {} unparseable DNAT rule(s); refusing to verify published ports: {}",
            firewall.unparsed_dnat_rules.len(),
            describe_unparsed_dnat_rules(&firewall.unparsed_dnat_rules)
        ));
        return PortsCheck::new(false, public_ip_override);
    }
    match resolve_public_ip(public_ip_override, &firewall.dnat_rules, uplink) {
        Err(err) => {
            findings.missing.push(err.to_string());
            PortsCheck::new(false, public_ip_override)
        }
        Ok(public_ip) => verify_ports(subject, firewall, public_ip, findings),
    }
}

fn verify_ports(
    subject: &CheckSubject<'_>,
    firewall: &FirewallStatus,
    public_ip: Ipv4Addr,
    findings: &mut Findings,
) -> PortsCheck {
    let recommended_sources = sources_or_any(subject.explicit_sources);
    let renderer =
        PortRuleRenderer::new(subject.vm_name, &recommended_sources, subject.ip, public_ip);
    let mut check = PortsCheck::new(false, Some(public_ip));
    let mut all_ports_present = true;

    for spec in subject.ports {
        let (dnat_matched, conflicts) =
            match_live_dnat(subject, spec, &firewall.dnat_rules, public_ip);
        let dnat_needed = !dnat_matched && conflicts.is_empty();
        for conflict in conflicts {
            findings.conflicts.push(describe_dnat_conflict(&conflict));
            check.port_conflicts.push(conflict);
        }
        let forward_matched = firewall
            .forward_allows
            .iter()
            .any(|rule| forward_allows_port(rule, subject, spec));

        if dnat_needed {
            all_ports_present = false;
            findings.missing.push(format!(
                "missing nftables prerouting DNAT rule for {}/{} ({public_ip}:{} -> {}:{})",
                spec.proto.as_str(),
                spec.host_port,
                spec.host_port,
                subject.ip,
                spec.guest_port
            ));
            check.prerouting_rules.push(renderer.prerouting_rule(spec));
        }
        if !forward_matched {
            all_ports_present = false;
            findings.missing.push(format!(
                "missing nftables forward accept rule for {}/{} -> {}:{}",
                spec.proto.as_str(),
                spec.guest_port,
                subject.ip,
                spec.guest_port
            ));
            check.forward_rules.push(renderer.forward_rule(spec));
        }
    }

    if !check.forward_rules.is_empty() || !check.prerouting_rules.is_empty() {
        check.firewall_commands = FirewallTargets::detect(firewall).apply_commands();
    }
    check.ok = all_ports_present && check.port_conflicts.is_empty();
    check
}

/// Compare one requested port with the live DNAT rules. Return whether a live rule already
/// publishes the port to the VM for the requested sources, and the conflicting live rules.
fn match_live_dnat(
    subject: &CheckSubject<'_>,
    spec: &PublishPortSpec,
    dnat_rules: &[FirewallDnatRule],
    public_ip: Ipv4Addr,
) -> (bool, Vec<PublishPortConflict>) {
    let mut dnat_matched = false;
    let mut conflicts = Vec::new();
    for rule in dnat_rules {
        match dnat_relation(rule, spec, public_ip, subject.ip) {
            DnatRelation::Unrelated => {}
            DnatRelation::SameTarget => {
                dnat_matched |=
                    nft_sources_cover_requested(&rule.sources, subject.explicit_sources);
            }
            DnatRelation::Conflict => {
                conflicts.push(PublishPortConflict::from_rule(rule, spec, subject.ip));
            }
        }
    }
    (dnat_matched, conflicts)
}

fn describe_dnat_conflict(conflict: &PublishPortConflict) -> String {
    format!(
        "host port {}/{} conflicts with existing DNAT rule -> {}:{} in {}::{} (comment={}, handle={:?})",
        conflict.proto,
        conflict.host_port,
        conflict.existing_dest_ip,
        conflict.existing_guest_port,
        conflict.existing_table,
        conflict.existing_chain,
        conflict.existing_comment.as_deref().unwrap_or("-"),
        conflict.existing_handle
    )
}

fn forward_allows_port(
    rule: &FirewallForwardAllow,
    subject: &CheckSubject<'_>,
    spec: &PublishPortSpec,
) -> bool {
    rule.dest_ip == subject.ip
        && rule.proto == spec.proto.as_str()
        && rule.dest_port == spec.guest_port
        && nft_sources_cover_requested(&rule.sources, subject.explicit_sources)
}

fn nft_sources_cover_requested(rule_sources: &[String], requested: &[String]) -> bool {
    if requested.is_empty() || rule_sources.is_empty() {
        return true;
    }
    if rule_sources.iter().any(|s| s == "0.0.0.0/0") {
        return true;
    }
    requested
        .iter()
        .all(|req| rule_sources.iter().any(|s| s == req))
}
