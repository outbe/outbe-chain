//! Parse forward-hook accept rules into per-port [`FirewallForwardAllow`] entries.

use std::net::Ipv4Addr;

use serde_json::Value;

use super::{
    expr::{
        extract_nft_ipv4_sources, extract_nft_port_ranges_checked, is_expandable_range,
        payload_match, NftRuleMeta, PayloadMatch,
    },
    FirewallForwardAllow,
};

/// Match fields of one forward accept rule.
#[derive(Default)]
struct ForwardMatches {
    dest_ip: Option<Ipv4Addr>,
    proto: Option<String>,
    dest_ports: Vec<u16>,
    sources: Vec<String>,
}

pub(super) fn parse_forward_allow_rule(
    meta: &NftRuleMeta<'_>,
    exprs: &[Value],
) -> Option<Vec<FirewallForwardAllow>> {
    let has_accept = exprs.iter().any(|e| e.get("accept").is_some());
    if !has_accept {
        return None;
    }

    let mut matches = ForwardMatches::default();
    for m in exprs.iter().filter_map(|e| payload_match(e, "==")) {
        if matches!(m.op, "==" | "in") {
            matches.record(&m);
        }
    }

    let dest_ip = matches.dest_ip?;
    let proto = matches.proto?;
    if matches.dest_ports.is_empty() {
        return None;
    }

    Some(
        matches
            .dest_ports
            .into_iter()
            .map(|dest_port| FirewallForwardAllow {
                family: meta.family.to_owned(),
                table: meta.table.to_owned(),
                chain: meta.chain.to_owned(),
                dest_ip,
                proto: proto.clone(),
                dest_port,
                sources: matches.sources.clone(),
                comment: meta.comment.map(str::to_owned),
            })
            .collect(),
    )
}

impl ForwardMatches {
    fn record(&mut self, m: &PayloadMatch<'_>) {
        match (m.protocol, m.field) {
            ("ip", "daddr") => {
                if let Some(s) = m.right.as_str() {
                    self.dest_ip = s.parse::<Ipv4Addr>().ok();
                }
            }
            ("ip", "saddr") => {
                self.sources.extend(extract_nft_ipv4_sources(m.right));
            }
            ("tcp" | "udp", "dport") => {
                self.proto = Some(m.protocol.to_owned());
                self.record_dest_ports(m.right);
            }
            _ => {}
        }
    }

    /// Record the ports of a `dport` match. A large range records only its first port. An
    /// unparseable match records no port.
    fn record_dest_ports(&mut self, right: &Value) {
        let Ok(ranges) = extract_nft_port_ranges_checked(right) else {
            return;
        };
        for (start, end) in ranges {
            let last = if is_expandable_range(start, end) {
                end
            } else {
                start
            };
            self.dest_ports.extend(start..=last);
        }
    }
}
