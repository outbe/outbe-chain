//! Classification of foreign nftables chains from `nft -j list ruleset` JSON.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use super::{
    egress::{rule_provides_egress, EgressMatch, EgressTarget},
    Ipv4Subnet, NAT_TABLE_FAMILY, NAT_TABLE_NAME,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardChainClass {
    /// `policy accept` and no rule with `drop`, `reject`, `jump`, `goto`, or `vmap`.
    PureAccept,
    /// `policy drop`, or contains at least one rule with `drop`, `reject`, `jump`, `goto`, or `vmap`.
    BlocksOrUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForeignForwardChain {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub priority: String,
    pub policy: String,
    pub class: ForwardChainClass,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExistingEgress {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PartialEgress {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub kind: String,
    pub narrowing: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NftInspection {
    pub foreign_forward_chains: Vec<ForeignForwardChain>,
    pub blocks_or_unknown_chains: Vec<ForeignForwardChain>,
    pub existing_egress: Vec<ExistingEgress>,
    pub partial_egress: Vec<PartialEgress>,
    pub docker_or_libvirt_chains: Vec<String>,
}

/// Parse structured `nft -j list ruleset` JSON output.
///
/// Classifies every foreign `ip`/`inet` `forward` base chain into `PureAccept` vs
/// `BlocksOrUnknown` (policy drop OR any rule containing `drop`, `reject`, `jump`,
/// `goto`, or `vmap`), and detects existing foreign `postrouting` masquerade/snat
/// rules that already provide egress for `subnet` towards `uplink`.
pub(crate) fn inspect_nft_json(
    json_str: &str,
    bridge: &str,
    subnet: Ipv4Subnet,
    uplink: &str,
) -> Result<NftInspection> {
    let root: Value = serde_json::from_str(json_str.trim())
        .wrap_err("parse `nft -j list ruleset` JSON output")?;
    let items = root
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre::eyre!("`nft -j list ruleset` JSON missing `nftables` array"))?;

    let index = RulesetIndex::build(items);
    let target = EgressTarget {
        bridge,
        subnet,
        uplink,
    };
    let mut inspection = NftInspection::default();
    for (key, meta) in index.chains.iter().filter(|(_, meta)| meta.is_foreign_ip()) {
        let rules = index.rules_for(key);
        match meta.hook.as_deref() {
            Some("forward") => {
                record_forward_chain(&mut inspection, classify_forward_chain(meta, rules))
            }
            Some("postrouting") => record_postrouting_egress(&mut inspection, meta, rules, &target),
            _ => {}
        }
    }
    inspection.docker_or_libvirt_chains = index.docker_or_libvirt.into_iter().collect();
    Ok(inspection)
}

/// `(family, table, chain)` identity of one nftables chain.
type ChainKey = (String, String, String);

struct ChainMeta {
    family: String,
    table: String,
    chain: String,
    hook: Option<String>,
    priority: String,
    policy: String,
}

impl ChainMeta {
    fn from_json(chain: &Map<String, Value>) -> Option<Self> {
        let (family, table, name) = chain_key(chain, "name")?;
        Some(Self {
            family,
            table,
            chain: name,
            hook: chain.get("hook").and_then(Value::as_str).map(str::to_owned),
            priority: priority_label(chain.get("prio")),
            policy: chain
                .get("policy")
                .and_then(Value::as_str)
                .unwrap_or("accept")
                .to_owned(),
        })
    }

    fn key(&self) -> ChainKey {
        (self.family.clone(), self.table.clone(), self.chain.clone())
    }

    /// True for `ip`/`inet` chains outside our own NAT table.
    fn is_foreign_ip(&self) -> bool {
        let is_own_table = self.family == NAT_TABLE_FAMILY && self.table == NAT_TABLE_NAME;
        !is_own_table && matches!(self.family.as_str(), "ip" | "inet")
    }

    fn is_docker_or_libvirt(&self) -> bool {
        let is_legacy_forward = self.table == "filter" && self.chain == "FORWARD";
        is_docker_or_libvirt_chain(&self.chain) || is_legacy_forward
    }

    fn existing_egress(&self, kind: &str) -> ExistingEgress {
        ExistingEgress {
            family: self.family.clone(),
            table: self.table.clone(),
            chain: self.chain.clone(),
            kind: kind.to_owned(),
        }
    }

    fn partial_egress(&self, kind: &str, narrowing: Vec<String>) -> PartialEgress {
        PartialEgress {
            family: self.family.clone(),
            table: self.table.clone(),
            chain: self.chain.clone(),
            kind: kind.to_owned(),
            narrowing,
        }
    }
}

/// Chains and their rules from one ruleset dump, indexed by chain identity.
#[derive(Default)]
struct RulesetIndex<'a> {
    chains: BTreeMap<ChainKey, ChainMeta>,
    rules_by_chain: BTreeMap<ChainKey, Vec<&'a Value>>,
    docker_or_libvirt: BTreeSet<String>,
}

impl<'a> RulesetIndex<'a> {
    fn build(items: &'a [Value]) -> Self {
        let mut index = Self::default();
        for item in items {
            index.record_item(item);
        }
        index
    }

    fn record_item(&mut self, item: &'a Value) {
        if let Some(chain) = item.get("chain").and_then(Value::as_object) {
            self.record_chain(chain);
        } else if let Some(rule) = item.get("rule").and_then(Value::as_object) {
            if let Some(key) = chain_key(rule, "chain") {
                self.rules_by_chain.entry(key).or_default().push(item);
            }
        }
    }

    fn record_chain(&mut self, chain: &Map<String, Value>) {
        let Some(meta) = ChainMeta::from_json(chain) else {
            return;
        };
        if meta.is_docker_or_libvirt() {
            self.docker_or_libvirt
                .insert(format!("{} {}::{}", meta.family, meta.table, meta.chain));
        }
        self.chains.insert(meta.key(), meta);
    }

    fn rules_for(&self, key: &ChainKey) -> &[&'a Value] {
        self.rules_by_chain
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Read `(family, table, <name_field>)` from a chain or rule object.
fn chain_key(obj: &Map<String, Value>, name_field: &str) -> Option<ChainKey> {
    let field = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_owned);
    Some((field("family")?, field("table")?, field(name_field)?))
}

fn priority_label(prio: Option<&Value>) -> String {
    match prio {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "unknown".to_owned(),
    }
}

/// Return the expression list of one `{"rule": {..., "expr": [...]}}` item.
fn rule_exprs(rule_item: &Value) -> Option<&Vec<Value>> {
    rule_item
        .get("rule")
        .and_then(|r| r.get("expr"))
        .and_then(Value::as_array)
}

fn classify_forward_chain(meta: &ChainMeta, rules: &[&Value]) -> ForeignForwardChain {
    let mut reasons = BTreeSet::new();
    if meta.policy != "accept" {
        reasons.insert(format!("policy {}", meta.policy));
    }
    for exprs in rules.iter().filter_map(|rule| rule_exprs(rule)) {
        collect_non_accept_verdicts(exprs, &mut reasons);
    }
    let class = if reasons.is_empty() {
        ForwardChainClass::PureAccept
    } else {
        ForwardChainClass::BlocksOrUnknown
    };
    ForeignForwardChain {
        family: meta.family.clone(),
        table: meta.table.clone(),
        chain: meta.chain.clone(),
        priority: meta.priority.clone(),
        policy: meta.policy.clone(),
        class,
        reasons: reasons.into_iter().collect(),
    }
}

fn record_forward_chain(inspection: &mut NftInspection, info: ForeignForwardChain) {
    if info.class == ForwardChainClass::BlocksOrUnknown {
        inspection.blocks_or_unknown_chains.push(info.clone());
    }
    inspection.foreign_forward_chains.push(info);
}

/// Record full egress once per chain and every partial egress rule.
fn record_postrouting_egress(
    inspection: &mut NftInspection,
    meta: &ChainMeta,
    rules: &[&Value],
    target: &EgressTarget<'_>,
) {
    let mut full_recorded = false;
    for exprs in rules.iter().filter_map(|rule| rule_exprs(rule)) {
        match rule_provides_egress(exprs, target) {
            EgressMatch::Full(kind) if !full_recorded => {
                full_recorded = true;
                inspection.existing_egress.push(meta.existing_egress(kind));
            }
            EgressMatch::Partial(kind, narrowing) => {
                inspection
                    .partial_egress
                    .push(meta.partial_egress(kind, narrowing));
            }
            EgressMatch::Full(_) | EgressMatch::None => {}
        }
    }
}

fn collect_non_accept_verdicts(exprs: &[Value], reasons: &mut BTreeSet<String>) {
    for expr in exprs {
        collect_non_accept_verdicts_value(expr, reasons);
    }
}

fn collect_non_accept_verdicts_value(val: &Value, reasons: &mut BTreeSet<String>) {
    match val {
        Value::Object(map) => {
            for (key, inner) in map {
                match non_accept_verdict_reason(key, inner) {
                    Some(reason) => {
                        reasons.insert(reason);
                    }
                    None => collect_non_accept_verdicts_value(inner, reasons),
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_non_accept_verdicts_value(item, reasons);
            }
        }
        _ => {}
    }
}

/// Describe a verdict key that can drop or divert forwarded traffic.
fn non_accept_verdict_reason(key: &str, val: &Value) -> Option<String> {
    let target = || {
        val.get("target")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };
    match key {
        "drop" => Some("drop rule".to_owned()),
        "reject" => Some("reject rule".to_owned()),
        "jump" => Some(format!("jump to {}", target())),
        "goto" => Some(format!("goto {}", target())),
        "vmap" => Some("vmap verdict".to_owned()),
        _ => None,
    }
}

fn is_docker_or_libvirt_chain(chain: &str) -> bool {
    chain.starts_with("DOCKER") || chain.starts_with("LIBVIRT")
}
