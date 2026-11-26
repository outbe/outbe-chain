//! Decide whether one foreign `postrouting` rule already provides VM egress.

use std::net::Ipv4Addr;

use serde_json::{Map, Value};

use super::Ipv4Subnet;

/// The bridge traffic that needs egress: its interface, its subnet, and the uplink.
pub(super) struct EgressTarget<'a> {
    pub(super) bridge: &'a str,
    pub(super) subnet: Ipv4Subnet,
    pub(super) uplink: &'a str,
}

pub(super) enum EgressMatch {
    None,
    Full(&'static str),
    Partial(&'static str, Vec<String>),
}

/// The effect of one rule expression on the egress decision.
enum ExprEffect {
    /// A `masquerade` or `snat` statement.
    Nat(&'static str),
    /// The expression does not narrow bridge egress.
    Neutral,
    /// The expression selects the bridge as the traffic source.
    BridgeSource,
    /// The expression narrows the rule to part of the bridge egress.
    Narrowing(String),
    /// The expression excludes bridge egress from the rule.
    Excludes,
}

/// Check whether a foreign `postrouting` rule provides full or partial masquerade/SNAT
/// for `subnet` towards `uplink`.
///
/// Only rules with no extra narrowing expressions (beyond bridge source, uplink oifname,
/// or non-bridge destination, plus benign `counter`/`log`) count as `Full`. Any extra
/// match (L4 protocol, ports, marks, ct state, specific daddr, limits, etc.) is classified
/// as `Partial` so apply never silently no-ops on incomplete egress.
pub(super) fn rule_provides_egress(exprs: &[Value], target: &EgressTarget<'_>) -> EgressMatch {
    let mut nat_kind: Option<&'static str> = None;
    let mut matches_bridge_source = false;
    let mut narrowing = Vec::new();

    for expr in exprs {
        match classify_expr(expr, target) {
            ExprEffect::Nat(kind) => nat_kind = Some(kind),
            ExprEffect::Neutral => {}
            ExprEffect::BridgeSource => matches_bridge_source = true,
            ExprEffect::Narrowing(reason) => narrowing.push(reason),
            ExprEffect::Excludes => return EgressMatch::None,
        }
    }

    match (matches_bridge_source, nat_kind) {
        (true, Some(kind)) if narrowing.is_empty() => EgressMatch::Full(kind),
        (true, Some(kind)) => EgressMatch::Partial(kind, narrowing),
        _ => EgressMatch::None,
    }
}

fn classify_expr(expr: &Value, target: &EgressTarget<'_>) -> ExprEffect {
    if let Some(kind) = ["masquerade", "snat"]
        .into_iter()
        .find(|key| expr.get(key).is_some())
    {
        return ExprEffect::Nat(kind);
    }
    if expr.get("counter").is_some() || expr.get("log").is_some() {
        return ExprEffect::Neutral;
    }
    match expr.get("match").and_then(Value::as_object) {
        Some(m) => classify_match(m, target),
        None => ExprEffect::Narrowing(first_key(expr).to_owned()),
    }
}

fn classify_match(m: &Map<String, Value>, target: &EgressTarget<'_>) -> ExprEffect {
    let op = m.get("op").and_then(Value::as_str).unwrap_or("==");
    let (Some(left), Some(right)) = (m.get("left"), m.get("right")) else {
        return ExprEffect::Narrowing("match".to_owned());
    };
    if let Some(payload) = left.get("payload").and_then(Value::as_object) {
        return classify_payload_match(payload, op, right, target.subnet);
    }
    if let Some(meta) = left.get("meta").and_then(Value::as_object) {
        return classify_meta_match(meta, op, right, target);
    }
    ExprEffect::Narrowing(format!("match {}", first_key(left)))
}

fn classify_payload_match(
    payload: &Map<String, Value>,
    op: &str,
    right: &Value,
    subnet: Ipv4Subnet,
) -> ExprEffect {
    let proto = payload.get("protocol").and_then(Value::as_str);
    let field = payload.get("field").and_then(Value::as_str);
    match (proto, field) {
        (Some("ip"), Some("saddr")) => {
            let covers_bridge = extract_ipv4_prefixes(right)
                .iter()
                .any(|p| p.covers(subnet));
            classify_source_match(op, covers_bridge, "ip.saddr")
        }
        (Some("ip"), Some("daddr")) => {
            classify_daddr_match(op, &extract_ipv4_prefixes(right), subnet)
        }
        _ => ExprEffect::Narrowing(format!(
            "match {}.{}",
            proto.unwrap_or("payload"),
            field.unwrap_or("?")
        )),
    }
}

fn classify_meta_match(
    meta: &Map<String, Value>,
    op: &str,
    right: &Value,
    target: &EgressTarget<'_>,
) -> ExprEffect {
    let key = meta.get("key").and_then(Value::as_str);
    match key {
        Some("iifname") => {
            let names_bridge = extract_string_values(right)
                .iter()
                .any(|s| s == target.bridge);
            classify_source_match(op, names_bridge, "meta.iifname")
        }
        Some("oifname") => classify_oifname_match(op, &extract_string_values(right), target),
        _ => ExprEffect::Narrowing(format!("match meta.{}", key.unwrap_or("?"))),
    }
}

/// Classify a source selector (`ip saddr` or `iifname`) by whether it names the bridge.
fn classify_source_match(op: &str, names_bridge: bool, label: &str) -> ExprEffect {
    match (op, names_bridge) {
        ("==", true) => ExprEffect::BridgeSource,
        ("==", false) | ("!=", true) => ExprEffect::Excludes,
        _ => ExprEffect::Narrowing(format!("match {label} {op}")),
    }
}

fn classify_daddr_match(op: &str, prefixes: &[Ipv4Subnet], subnet: Ipv4Subnet) -> ExprEffect {
    let covers_bridge = prefixes.iter().any(|p| p.covers(subnet));
    let only_bridge = !prefixes.is_empty() && prefixes.iter().all(|p| subnet.covers(*p));
    match op {
        // Hairpin rule targeting the bridge subnet itself: not internet egress.
        "==" if covers_bridge => ExprEffect::Excludes,
        // Excluding the bridge subnet itself (`ip daddr != 10.77.0.0/24`) still covers internet egress.
        "!=" if only_bridge => ExprEffect::Neutral,
        _ => ExprEffect::Narrowing(format!("match ip.daddr {op}")),
    }
}

fn classify_oifname_match(op: &str, ifaces: &[String], target: &EgressTarget<'_>) -> ExprEffect {
    let names_uplink = ifaces.iter().any(|s| s == target.uplink);
    let only_local = !ifaces.is_empty() && ifaces.iter().all(|s| s == target.bridge || s == "lo");
    match op {
        "==" if names_uplink => ExprEffect::Neutral,
        "==" => ExprEffect::Excludes,
        "!=" if names_uplink => ExprEffect::Excludes,
        "!=" if only_local => ExprEffect::Neutral,
        _ => ExprEffect::Narrowing(format!("match meta.oifname {op}")),
    }
}

/// Name an unrecognized expression by its first JSON key.
fn first_key(val: &Value) -> &str {
    val.as_object()
        .and_then(|o| o.keys().next())
        .map(String::as_str)
        .unwrap_or("expr")
}

fn extract_ipv4_prefixes(val: &Value) -> Vec<Ipv4Subnet> {
    match val {
        Value::Object(map) => prefixes_from_object(map),
        Value::Array(arr) => arr.iter().flat_map(extract_ipv4_prefixes).collect(),
        Value::String(s) => parse_prefix_string(s).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Read a `{"prefix": {...}}` object or recurse into a `{"set": ...}` object.
fn prefixes_from_object(map: &Map<String, Value>) -> Vec<Ipv4Subnet> {
    if let Some(prefix) = map.get("prefix").and_then(Value::as_object) {
        return parse_prefix_object(prefix).into_iter().collect();
    }
    map.get("set")
        .map(extract_ipv4_prefixes)
        .unwrap_or_default()
}

fn parse_prefix_object(prefix: &Map<String, Value>) -> Option<Ipv4Subnet> {
    let addr = prefix.get("addr")?.as_str()?.parse::<Ipv4Addr>().ok()?;
    let len = u8::try_from(prefix.get("len")?.as_u64()?).ok()?;
    Ipv4Subnet::from_nft_prefix(addr, len)
}

/// Parse `a.b.c.d/len` or a bare `a.b.c.d` (as `/32`).
fn parse_prefix_string(s: &str) -> Option<Ipv4Subnet> {
    match s.split_once('/') {
        Some((addr_s, len_s)) => {
            Ipv4Subnet::from_nft_prefix(addr_s.parse().ok()?, len_s.parse().ok()?)
        }
        None => Ipv4Subnet::from_nft_prefix(s.parse().ok()?, 32),
    }
}

fn extract_string_values(val: &Value) -> Vec<String> {
    match val {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr.iter().flat_map(extract_string_values).collect(),
        Value::Object(map) => map
            .get("set")
            .map(extract_string_values)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
