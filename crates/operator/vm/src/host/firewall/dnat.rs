//! Strict DNAT rule parsing. A rule that cannot be resolved to static ports and a static target
//! gives a reason string, and the caller records it as an unparsed DNAT rule (F4).

use std::net::Ipv4Addr;

use serde_json::Value;

use super::{
    expr::{
        extract_nft_ipv4_sources, extract_nft_port_ranges_checked, is_expandable_range,
        nonzero_port, payload_match, NftRuleMeta, PayloadMatch,
    },
    FirewallDnatRule,
};

/// Static target of a `dnat` statement.
#[derive(Clone, Copy)]
struct DnatTarget {
    addr: Ipv4Addr,
    port: Option<u16>,
}

/// State of the `ip daddr` matches of a DNAT rule.
#[derive(Clone, Copy, Default)]
enum DaddrMatch {
    /// No `ip daddr` match yet.
    #[default]
    Unset,
    /// Exactly one `ip daddr == <ipv4>` match.
    Exact(Ipv4Addr),
    /// A set, prefix, named set, other operator, or more than one `ip daddr` match.
    Ambiguous,
}

/// Statements and matches of one DNAT rule, collected in expression order.
#[derive(Default)]
struct DnatRuleParts {
    target: Option<DnatTarget>,
    daddr: DaddrMatch,
    proto: Option<String>,
    host_port_ranges: Vec<(u16, u16)>,
    sources: Vec<String>,
}

pub(super) fn value_contains_dnat(val: &Value) -> bool {
    match val {
        Value::Object(map) => {
            if map.contains_key("dnat") {
                return true;
            }
            map.values().any(value_contains_dnat)
        }
        Value::Array(arr) => arr.iter().any(value_contains_dnat),
        _ => false,
    }
}

pub(super) fn parse_dnat_rule(
    meta: &NftRuleMeta<'_>,
    exprs: &[Value],
) -> Result<Vec<FirewallDnatRule>, String> {
    let mut parts = DnatRuleParts::default();
    for expr in exprs {
        parts.record(expr)?;
    }
    parts.into_rules(meta)
}

impl DnatRuleParts {
    fn record(&mut self, expr: &Value) -> Result<(), String> {
        if let Some(dnat_val) = expr.get("dnat") {
            if self.target.is_some() {
                return Err("rule contains multiple dnat statements".to_owned());
            }
            self.target = Some(parse_dnat_target(dnat_val)?);
            return Ok(());
        }
        if value_contains_dnat(expr) {
            return Err(format!("nested/map dnat expression: {expr}"));
        }
        match payload_match(expr, "") {
            Some(m) => self.record_match(&m),
            None => Ok(()),
        }
    }

    fn record_match(&mut self, m: &PayloadMatch<'_>) -> Result<(), String> {
        match (m.protocol, m.field) {
            ("ip", "daddr") => self.record_daddr(m),
            ("ip", "saddr") => self.sources.extend(extract_nft_ipv4_sources(m.right)),
            ("tcp" | "udp", "dport") => self.record_dport(m)?,
            ("th", "dport") => {
                return Err("generic transport-header `th dport` match in DNAT rule".to_owned());
            }
            _ => {}
        }
        Ok(())
    }

    /// Keep the public IP only for exactly one `ip daddr == <ipv4>` match. Any other `ip daddr`
    /// match makes the public IP a wildcard.
    fn record_daddr(&mut self, m: &PayloadMatch<'_>) {
        let exact_ip = if m.op == "==" {
            m.right.as_str().and_then(|s| s.parse::<Ipv4Addr>().ok())
        } else {
            None
        };
        self.daddr = match (self.daddr, exact_ip) {
            (DaddrMatch::Unset, Some(ip)) => DaddrMatch::Exact(ip),
            _ => DaddrMatch::Ambiguous,
        };
    }

    fn record_dport(&mut self, m: &PayloadMatch<'_>) -> Result<(), String> {
        let (op, p) = (m.op, m.protocol);
        if !matches!(op, "==" | "in") {
            return Err(format!(
                "non-equality operator `{op}` on `{p} dport` in DNAT rule"
            ));
        }
        match &self.proto {
            Some(existing_proto) if existing_proto != p => {
                return Err(format!(
                    "conflicting transport protocols `{existing_proto}` and `{p}` in DNAT rule"
                ));
            }
            Some(_) => {}
            None => self.proto = Some(p.to_owned()),
        }
        self.host_port_ranges
            .extend(extract_nft_port_ranges_checked(m.right)?);
        Ok(())
    }

    fn into_rules(self, meta: &NftRuleMeta<'_>) -> Result<Vec<FirewallDnatRule>, String> {
        let target = self
            .target
            .ok_or_else(|| "DNAT rule missing top-level static `dnat` statement".to_owned())?;
        let proto = self.proto.ok_or_else(|| {
            "wildcard or non-standard DNAT rule without static tcp/udp `dport` match".to_owned()
        })?;
        if self.host_port_ranges.is_empty() {
            return Err("DNAT rule has empty `dport` match".to_owned());
        }
        let public_ip = match self.daddr {
            DaddrMatch::Exact(ip) => Some(ip),
            DaddrMatch::Unset | DaddrMatch::Ambiguous => None,
        };
        let entry = |host_port: u16, host_port_end: Option<u16>| FirewallDnatRule {
            family: meta.family.to_owned(),
            table: meta.table.to_owned(),
            chain: meta.chain.to_owned(),
            public_ip,
            proto: proto.clone(),
            host_port,
            host_port_end,
            dest_ip: target.addr,
            guest_port: target.port.unwrap_or(host_port),
            sources: self.sources.clone(),
            comment: meta.comment.map(str::to_owned),
            handle: meta.handle,
        };
        let mut rules = Vec::new();
        for (start, end) in self.host_port_ranges.iter().copied() {
            if is_expandable_range(start, end) {
                rules.extend((start..=end).map(|host_port| entry(host_port, None)));
            } else {
                rules.push(entry(start, Some(end)));
            }
        }
        Ok(rules)
    }
}

fn parse_dnat_target(dnat_val: &Value) -> Result<DnatTarget, String> {
    let dnat = dnat_val
        .as_object()
        .ok_or_else(|| format!("non-object dnat statement: {dnat_val}"))?;
    let addr_val = dnat
        .get("addr")
        .ok_or_else(|| "dnat statement missing `addr`".to_owned())?;
    let addr_str = addr_val
        .as_str()
        .ok_or_else(|| format!("dynamic or non-static dnat target `addr`: {addr_val}"))?;
    let addr = addr_str
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("non-IPv4 dnat target `addr`: `{addr_str}`"))?;
    let port = dnat.get("port").map(parse_dnat_port).transpose()?;
    Ok(DnatTarget { addr, port })
}

fn parse_dnat_port(port_val: &Value) -> Result<u16, String> {
    let p_u64 = port_val
        .as_u64()
        .ok_or_else(|| format!("dynamic or non-numeric dnat target `port`: {port_val}"))?;
    nonzero_port(p_u64).ok_or_else(|| format!("out-of-range dnat target `port`: {p_u64}"))
}
