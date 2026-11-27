//! Shared nftables JSON expression readers: rule identity, payload matches, IPv4 sources, and
//! `dport` port ranges.

use std::net::Ipv4Addr;

use serde_json::{Map, Number, Value};

/// Maximum inclusive port range size expanded into individual [`crate::host::FirewallDnatRule`] /
/// [`crate::host::FirewallForwardAllow`] entries; larger ranges are kept compact via `host_port_end`.
pub(super) const MAX_EXPANDED_PORT_RANGE: u16 = 1024;

/// Identity of one nftables rule: its table, chain, comment, and handle.
pub(super) struct NftRuleMeta<'a> {
    pub(super) family: &'a str,
    pub(super) table: &'a str,
    pub(super) chain: &'a str,
    pub(super) comment: Option<&'a str>,
    pub(super) handle: Option<u64>,
}

/// One nftables `match` expression whose left side is a packet `payload` field.
pub(super) struct PayloadMatch<'a> {
    pub(super) op: &'a str,
    pub(super) protocol: &'a str,
    pub(super) field: &'a str,
    pub(super) right: &'a Value,
}

/// Read a payload `match` expression. A missing `op` gives `default_op`. Any other expression
/// gives `None`.
pub(super) fn payload_match<'a>(expr: &'a Value, default_op: &'a str) -> Option<PayloadMatch<'a>> {
    let m = expr.get("match").and_then(Value::as_object)?;
    let op = m.get("op").and_then(Value::as_str).unwrap_or(default_op);
    let (left, right) = (m.get("left")?, m.get("right")?);
    let payload = left.get("payload").and_then(Value::as_object)?;
    Some(PayloadMatch {
        op,
        protocol: payload
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or(""),
        field: payload.get("field").and_then(Value::as_str).unwrap_or(""),
        right,
    })
}

pub(super) fn extract_nft_ipv4_sources(val: &Value) -> Vec<String> {
    match val {
        Value::String(s) => match s.parse::<Ipv4Addr>() {
            Ok(ip) => vec![format!("{ip}/32")],
            Err(_) => vec![s.clone()],
        },
        Value::Object(map) => object_ipv4_sources(map),
        Value::Array(arr) => ipv4_sources_of_items(arr),
        _ => Vec::new(),
    }
}

/// Read the sources of a `prefix` object or a `set` object. Other objects give no sources.
fn object_ipv4_sources(map: &Map<String, Value>) -> Vec<String> {
    if let Some(cidr) = map
        .get("prefix")
        .and_then(Value::as_object)
        .and_then(prefix_cidr)
    {
        return vec![cidr];
    }
    map.get("set")
        .and_then(Value::as_array)
        .map(|set| ipv4_sources_of_items(set))
        .unwrap_or_default()
}

fn prefix_cidr(prefix: &Map<String, Value>) -> Option<String> {
    let addr = prefix.get("addr").and_then(Value::as_str)?;
    let len = prefix.get("len").and_then(Value::as_u64)?;
    Some(format!("{addr}/{len}"))
}

fn ipv4_sources_of_items(items: &[Value]) -> Vec<String> {
    items.iter().flat_map(extract_nft_ipv4_sources).collect()
}

/// Strictly parse an nftables `dport` RHS into inclusive `(start, end)` port ranges.
///
/// Supports single port numbers, `{"range": [start, end]}`, `{"set": [...]}` and arrays of those.
/// Fails closed on named sets (`"@ports"`), maps, concatenations, or malformed values (F4).
pub(super) fn extract_nft_port_ranges_checked(val: &Value) -> Result<Vec<(u16, u16)>, String> {
    match val {
        Value::Number(n) => single_port_range(n, val),
        Value::Object(map) => object_port_ranges(map, val),
        Value::Array(arr) => port_ranges_of_items(arr, "empty port array expression"),
        other => Err(format!("unparseable nftables dport expression: {other}")),
    }
}

fn single_port_range(n: &Number, val: &Value) -> Result<Vec<(u16, u16)>, String> {
    let p_u64 = n
        .as_u64()
        .ok_or_else(|| format!("non-integer port expression: {val}"))?;
    let p = nonzero_port(p_u64).ok_or_else(|| format!("out-of-range port number: {p_u64}"))?;
    Ok(vec![(p, p)])
}

fn object_port_ranges(map: &Map<String, Value>, val: &Value) -> Result<Vec<(u16, u16)>, String> {
    if let Some(range_arr) = map.get("range").and_then(Value::as_array) {
        return explicit_port_range(range_arr, val);
    }
    if let Some(set) = map.get("set").and_then(Value::as_array) {
        return port_ranges_of_items(set, "empty port set expression");
    }
    Err(format!("unsupported nftables dport expression: {val}"))
}

fn explicit_port_range(range_arr: &[Value], val: &Value) -> Result<Vec<(u16, u16)>, String> {
    let [start, end] = range_arr else {
        return Err(format!("malformed port range expression: {val}"));
    };
    let start = range_bound(start).ok_or_else(|| format!("invalid port range start in {val}"))?;
    let end = range_bound(end).ok_or_else(|| format!("invalid port range end in {val}"))?;
    if start > end {
        return Err(format!("inverted port range {start}..={end} in {val}"));
    }
    Ok(vec![(start, end)])
}

fn range_bound(bound: &Value) -> Option<u16> {
    bound.as_u64().and_then(nonzero_port)
}

/// Convert a JSON port number to a port in `1..=65535`.
pub(super) fn nonzero_port(raw: u64) -> Option<u16> {
    u16::try_from(raw).ok().filter(|&p| p > 0)
}

fn port_ranges_of_items(items: &[Value], empty_message: &str) -> Result<Vec<(u16, u16)>, String> {
    if items.is_empty() {
        return Err(empty_message.to_owned());
    }
    let mut out = Vec::new();
    for item in items {
        out.extend(extract_nft_port_ranges_checked(item)?);
    }
    Ok(out)
}

/// Returns true if an inclusive port range is small enough to expand into single ports.
pub(super) fn is_expandable_range(start: u16, end: u16) -> bool {
    end - start <= MAX_EXPANDED_PORT_RANGE
}
