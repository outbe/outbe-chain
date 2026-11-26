//! Read-only checks against the dnsmasq leases file.

use std::{collections::BTreeMap, net::Ipv4Addr};

use eyre::{ensure, Result};

/// Parse standard dnsmasq lease lines (`<expiry> <mac> <ip> <hostname> <client-id>`).
fn parse_dnsmasq_leases(existing_leases: &str) -> BTreeMap<Ipv4Addr, String> {
    existing_leases
        .lines()
        .filter_map(parse_lease_line)
        .collect()
}

/// Parse one lease line into `(ip, lowercase_mac)`. Blank, comment, and short lines give `None`.
fn parse_lease_line(line: &str) -> Option<(Ipv4Addr, String)> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    let cols: Vec<&str> = trimmed.split_whitespace().collect();
    let ip = cols.get(2)?.parse::<Ipv4Addr>().ok()?;
    Some((ip, cols[1].to_ascii_lowercase()))
}

/// Look up the leased IPv4 address for `mac` in a dnsmasq leases file, if present.
pub fn lookup_lease_ip_by_mac(existing_leases: &str, mac: &str) -> Option<Ipv4Addr> {
    let needle = mac.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    parse_dnsmasq_leases(existing_leases)
        .into_iter()
        .find_map(|(ip, lease_mac)| (lease_mac == needle).then_some(ip))
}

/// Refuse when any requested bridge IP has an active lease for a MAC other than the exact
/// requested MAC+IP pair.
///
/// Standard dnsmasq lease format: `<expiry> <mac> <ip> <hostname> <client-id>`.
pub fn check_dnsmasq_lease_conflicts(
    existing_leases: &str,
    requested: &[(String, Ipv4Addr)],
) -> Result<()> {
    let leases = parse_dnsmasq_leases(existing_leases);
    for (our_mac, our_ip) in requested {
        let Some(lease_mac) = leases.get(our_ip) else {
            continue;
        };
        let our_mac_lc = our_mac.to_ascii_lowercase();
        let exact_match = !our_mac_lc.is_empty() && *lease_mac == our_mac_lc;
        ensure!(
            exact_match,
            "bridge IP {our_ip} is currently leased to MAC {lease_mac} in dnsmasq leases"
        );
    }
    Ok(())
}

/// Count non-empty, non-comment lease lines in a dnsmasq leases file.
pub fn count_dnsmasq_leases(existing_leases: &str) -> usize {
    parse_dnsmasq_leases(existing_leases).len()
}
