//! Published-port rule logic shared by `outbe-up host check` and `outbe-up host publish`:
//! public IP resolution, live DNAT rule comparison, and `/etc/host-firewall.nft` rule rendering.

use std::{collections::BTreeSet, net::Ipv4Addr};

use eyre::{bail, Result};

use super::{
    bridge::parse_ip_json_bridge_addr, probe::run_capture, FirewallDnatRule, FirewallStatus,
    PublishPortConflict, PublishPortSpec, UnparsedDnatRule,
};

pub(super) const HOST_FIREWALL_NFT_PATH: &str = "/etc/host-firewall.nft";
pub(super) const HOST_FIREWALL_SERVICE: &str = "host-firewall.service";

pub(super) fn resolve_public_ip(
    explicit: Option<Ipv4Addr>,
    dnat_rules: &[FirewallDnatRule],
    uplink: Option<&str>,
) -> Result<Ipv4Addr> {
    if let Some(ip) = explicit {
        return Ok(ip);
    }
    let existing_public_ips: BTreeSet<Ipv4Addr> = dnat_rules
        .iter()
        .filter_map(|rule| rule.public_ip)
        .collect();
    if existing_public_ips.len() == 1 {
        return Ok(*existing_public_ips.iter().next().unwrap());
    }
    if let Some(ip) = uplink.and_then(uplink_ipv4) {
        return Ok(ip);
    }
    bail!("could not determine host public IPv4 address automatically; pass `--public-ip <ipv4>`");
}

/// Read the first IPv4 address of the uplink interface.
fn uplink_ipv4(iface: &str) -> Option<Ipv4Addr> {
    let raw = run_capture("ip", &["-j", "-4", "addr", "show", "dev", iface])?;
    parse_ip_json_bridge_addr(&raw)?.1
}

/// Describe each unparsed DNAT rule on one line, joined with `; `.
pub(super) fn describe_unparsed_dnat_rules(rules: &[UnparsedDnatRule]) -> String {
    rules
        .iter()
        .map(|u| {
            format!(
                "{} {}::{} (handle={:?}, comment={}): {}",
                u.family,
                u.table,
                u.chain,
                u.handle,
                u.comment.as_deref().unwrap_or("-"),
                u.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// How a live DNAT rule relates to one requested published port.
pub(super) enum DnatRelation {
    /// The rule matches another protocol, another host port, or another public IP.
    Unrelated,
    /// The rule sends the requested host port to the requested guest IP and port.
    SameTarget,
    /// The rule claims the requested host port for another target or a port range.
    Conflict,
}

pub(super) fn dnat_relation(
    rule: &FirewallDnatRule,
    spec: &PublishPortSpec,
    public_ip: Ipv4Addr,
    guest_ip: Ipv4Addr,
) -> DnatRelation {
    let same_listener = rule.proto == spec.proto.as_str()
        && rule.covers_host_port(spec.host_port)
        && rule.public_ip.is_none_or(|rule_pub| rule_pub == public_ip);
    if !same_listener {
        return DnatRelation::Unrelated;
    }
    let same_target = rule.host_port_end.is_none()
        && rule.dest_ip == guest_ip
        && rule.guest_port == spec.guest_port;
    if same_target {
        DnatRelation::SameTarget
    } else {
        DnatRelation::Conflict
    }
}

impl PublishPortConflict {
    /// Record that live DNAT `rule` blocks publishing `spec` to `guest_ip`.
    pub(super) fn from_rule(
        rule: &FirewallDnatRule,
        spec: &PublishPortSpec,
        guest_ip: Ipv4Addr,
    ) -> Self {
        Self {
            proto: rule.proto.clone(),
            host_port: spec.host_port,
            requested_dest_ip: guest_ip,
            requested_guest_port: spec.guest_port,
            existing_table: format!("{} {}", rule.family, rule.table),
            existing_chain: rule.chain.clone(),
            existing_dest_ip: rule.dest_ip,
            existing_guest_port: rule.guest_port,
            existing_sources: rule.sources.clone(),
            existing_comment: rule.comment.clone(),
            existing_handle: rule.handle,
        }
    }
}

/// Tables of `/etc/host-firewall.nft` that receive the published-port rules.
pub(super) struct FirewallTargets {
    pub(super) forward_table: String,
    pub(super) forward_policy: String,
    pub(super) nat_table: String,
}

impl FirewallTargets {
    pub(super) fn detect(firewall: &FirewallStatus) -> Self {
        let (forward_table, forward_policy) = firewall
            .forward_chains
            .iter()
            .find(|c| c.table == "host_firewall" && c.chain == "forward")
            .or_else(|| firewall.forward_chains.iter().find(|c| c.policy == "drop"))
            .map(|c| (format!("{} {}", c.family, c.table), c.policy.clone()))
            .unwrap_or_else(|| ("inet host_firewall".to_owned(), "unknown".to_owned()));
        let nat_table = firewall
            .tables
            .iter()
            .find(|t| t.name == "host_firewall_nat")
            .map(|t| format!("{} {}", t.family, t.name))
            .unwrap_or_else(|| "ip host_firewall_nat".to_owned());
        Self {
            forward_table,
            forward_policy,
            nat_table,
        }
    }

    /// Operator steps that add the rendered rules and reload the host firewall.
    pub(super) fn apply_commands(&self) -> Vec<String> {
        let Self {
            forward_table,
            nat_table,
            ..
        } = self;
        vec![
            format!(
                "Add the forward rule(s) into `table {forward_table} {{ chain forward {{ ... }} }}` and the prerouting rule(s) into `table {nat_table} {{ chain prerouting {{ ... }} }}` inside `{HOST_FIREWALL_NFT_PATH}`"
            ),
            format!("sudo nft --check --file {HOST_FIREWALL_NFT_PATH}"),
            format!("sudo systemctl reload {HOST_FIREWALL_SERVICE}"),
        ]
    }
}

/// Renders the nftables rule lines that publish ports of one VM.
pub(super) struct PortRuleRenderer {
    sources_set: String,
    comment: String,
    guest_ip: Ipv4Addr,
    public_ip: Ipv4Addr,
}

impl PortRuleRenderer {
    pub(super) fn new(
        vm_name: &str,
        sources: &[String],
        guest_ip: Ipv4Addr,
        public_ip: Ipv4Addr,
    ) -> Self {
        Self {
            sources_set: format!("{{ {} }}", sources.join(", ")),
            comment: format!("outbe-up:{vm_name}"),
            guest_ip,
            public_ip,
        }
    }

    /// The `comment` tag of every rendered rule.
    pub(super) fn comment(&self) -> &str {
        &self.comment
    }

    pub(super) fn forward_rule(&self, spec: &PublishPortSpec) -> String {
        let Self {
            sources_set,
            comment,
            guest_ip,
            ..
        } = self;
        format!(
            "ip saddr {sources_set} ip daddr {guest_ip} ct status dnat {} dport {} accept comment \"{comment}\";",
            spec.proto.as_str(),
            spec.guest_port
        )
    }

    pub(super) fn prerouting_rule(&self, spec: &PublishPortSpec) -> String {
        let Self {
            sources_set,
            comment,
            guest_ip,
            public_ip,
        } = self;
        let dnat_target = if spec.host_port == spec.guest_port {
            guest_ip.to_string()
        } else {
            format!("{guest_ip}:{}", spec.guest_port)
        };
        format!(
            "ip daddr {public_ip} ip saddr {sources_set} {} dport {} dnat to {dnat_target} comment \"{comment}\";",
            spec.proto.as_str(),
            spec.host_port
        )
    }
}
