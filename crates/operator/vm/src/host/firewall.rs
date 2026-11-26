//! Live nftables inspection: tables, forward accept rules, DNAT rules, and outbound egress.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

use eyre::{Context, Result};
use serde_json::{Map, Value};

use super::{
    probe::run_privileged_capture, FirewallDnatRule, FirewallForwardAllow, FirewallStatus,
    FirewallTableRef, UnparsedDnatRule,
};
use crate::nat::{
    inspect_nft_json, ExistingEgress, Ipv4Subnet, PartialEgress, NAT_TABLE_FAMILY, NAT_TABLE_NAME,
};

mod dnat;
mod expr;
mod forward;

use dnat::{parse_dnat_rule, value_contains_dnat};
use expr::NftRuleMeta;
use forward::parse_forward_allow_rule;

/// Firewall status plus the full and partial egress providers for one bridge.
pub(super) type FirewallInspection = (FirewallStatus, Vec<ExistingEgress>, Vec<PartialEgress>);

/// `(family, table, chain)` key of an nftables base chain.
type ChainKey = (String, String, String);

pub(super) fn classify_egress_status(
    nft_accessible: bool,
    managed_nat_table_present: bool,
    existing_egress: &[ExistingEgress],
    partial_egress: &[PartialEgress],
) -> &'static str {
    if !nft_accessible {
        "unknown"
    } else if !existing_egress.is_empty() {
        "foreign_provided"
    } else if managed_nat_table_present {
        "managed"
    } else if !partial_egress.is_empty() {
        "partial"
    } else {
        "missing"
    }
}

pub(super) fn detect_firewall_and_egress(
    bridge_name: &str,
    bridge_subnet: Option<Ipv4Subnet>,
    uplink: Option<&str>,
) -> FirewallInspection {
    run_privileged_capture("nft", &["-j", "list", "ruleset"])
        .and_then(|json_str| {
            parse_firewall_and_egress_json(&json_str, bridge_name, bridge_subnet, uplink).ok()
        })
        .unwrap_or_else(|| (FirewallStatus::inaccessible(), Vec::new(), Vec::new()))
}

impl FirewallStatus {
    /// Status for a host whose nftables ruleset cannot be read or parsed.
    fn inaccessible() -> Self {
        Self {
            nft_accessible: false,
            tables: Vec::new(),
            forward_chains: Vec::new(),
            forward_allows: Vec::new(),
            dnat_rules: Vec::new(),
            unparsed_dnat_rules: Vec::new(),
            managed_nat_table_present: false,
        }
    }
}

/// Parse `nft -j list ruleset` JSON into [`FirewallStatus`] plus egress inspection results.
pub fn parse_firewall_and_egress_json(
    json_str: &str,
    bridge_name: &str,
    bridge_subnet: Option<Ipv4Subnet>,
    uplink: Option<&str>,
) -> Result<(FirewallStatus, Vec<ExistingEgress>, Vec<PartialEgress>)> {
    let root: Value = serde_json::from_str(json_str.trim())
        .wrap_err("parse `nft -j list ruleset` JSON output")?;
    let items = root
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre::eyre!("`nft -j list ruleset` JSON missing `nftables` array"))?;

    let mut inventory = TableInventory::default();
    for item in items {
        inventory.absorb(item);
    }
    let mut rules = RuleInventory::default();
    for rule in items
        .iter()
        .filter_map(|item| item.get("rule").and_then(Value::as_object))
    {
        rules.absorb(rule, &inventory.chain_hooks);
    }

    // Reuse `nat::inspect_nft_json` for exact forward chain classification & egress detection.
    let probe_subnet = bridge_subnet.unwrap_or(Ipv4Subnet {
        network: Ipv4Addr::new(10, 77, 0, 0),
        prefix_len: 24,
    });
    let probe_uplink = uplink.unwrap_or("eth0");
    let nat_insp = inspect_nft_json(json_str, bridge_name, probe_subnet, probe_uplink)?;

    let (existing_egress, partial_egress) = if bridge_subnet.is_some() && uplink.is_some() {
        (nat_insp.existing_egress, nat_insp.partial_egress)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok((
        FirewallStatus {
            nft_accessible: true,
            tables: inventory.tables,
            forward_chains: nat_insp.foreign_forward_chains,
            forward_allows: rules.forward_allows,
            dnat_rules: rules.dnat_rules,
            unparsed_dnat_rules: rules.unparsed_dnat_rules,
            managed_nat_table_present: inventory.managed_nat_table_present,
        },
        existing_egress,
        partial_egress,
    ))
}

/// Tables and base chain hooks of an nftables ruleset.
#[derive(Default)]
struct TableInventory {
    tables: Vec<FirewallTableRef>,
    seen_tables: BTreeSet<(String, String)>,
    managed_nat_table_present: bool,
    chain_hooks: BTreeMap<ChainKey, String>,
}

impl TableInventory {
    fn absorb(&mut self, item: &Value) {
        if let Some(tbl) = item.get("table").and_then(Value::as_object) {
            self.add_table(tbl);
        } else if let Some(chain) = item.get("chain").and_then(Value::as_object) {
            self.add_chain_hook(chain);
        }
    }

    fn add_table(&mut self, tbl: &Map<String, Value>) {
        let (Some(family), Some(name)) = (str_field(tbl, "family"), str_field(tbl, "name")) else {
            return;
        };
        if family == NAT_TABLE_FAMILY && name == NAT_TABLE_NAME {
            self.managed_nat_table_present = true;
        }
        if self
            .seen_tables
            .insert((family.to_owned(), name.to_owned()))
        {
            self.tables.push(FirewallTableRef {
                family: family.to_owned(),
                name: name.to_owned(),
            });
        }
    }

    fn add_chain_hook(&mut self, chain: &Map<String, Value>) {
        let (Some(family), Some(table), Some(name), Some(hook)) = (
            str_field(chain, "family"),
            str_field(chain, "table"),
            str_field(chain, "name"),
            str_field(chain, "hook"),
        ) else {
            return;
        };
        self.chain_hooks.insert(
            (family.to_owned(), table.to_owned(), name.to_owned()),
            hook.to_owned(),
        );
    }
}

/// Forward accept rules and DNAT rules of an nftables ruleset.
#[derive(Default)]
struct RuleInventory {
    forward_allows: Vec<FirewallForwardAllow>,
    dnat_rules: Vec<FirewallDnatRule>,
    unparsed_dnat_rules: Vec<UnparsedDnatRule>,
}

impl RuleInventory {
    fn absorb(&mut self, rule: &Map<String, Value>, chain_hooks: &BTreeMap<ChainKey, String>) {
        let (Some(family), Some(table), Some(chain)) = (
            str_field(rule, "family"),
            str_field(rule, "table"),
            str_field(rule, "chain"),
        ) else {
            return;
        };
        let Some(exprs) = rule.get("expr").and_then(Value::as_array) else {
            return;
        };
        let meta = NftRuleMeta {
            family,
            table,
            chain,
            comment: str_field(rule, "comment"),
            handle: rule.get("handle").and_then(Value::as_u64),
        };
        let hook = chain_hooks
            .get(&(family.to_owned(), table.to_owned(), chain.to_owned()))
            .map(String::as_str);

        if hook == Some("forward") {
            self.forward_allows
                .extend(parse_forward_allow_rule(&meta, exprs).unwrap_or_default());
        }
        if exprs.iter().any(value_contains_dnat) {
            self.absorb_dnat(&meta, exprs);
        }
    }

    fn absorb_dnat(&mut self, meta: &NftRuleMeta<'_>, exprs: &[Value]) {
        match parse_dnat_rule(meta, exprs) {
            Ok(parsed) => self.dnat_rules.extend(parsed),
            Err(reason) => self.unparsed_dnat_rules.push(UnparsedDnatRule {
                family: meta.family.to_owned(),
                table: meta.table.to_owned(),
                chain: meta.chain.to_owned(),
                handle: meta.handle,
                comment: meta.comment.map(str::to_owned),
                reason,
            }),
        }
    }
}

fn str_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}
