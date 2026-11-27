//! Read-only grammar and conflict checks on dnsmasq `--dhcp-hostsfile` lines.

use std::net::Ipv4Addr;

use eyre::{bail, ensure, Result};

use super::is_mac_address;
use crate::block_splice::{iter_line_spans, parse_managed_blocks};

/// One non-empty hosts file line with its comment removed.
struct HostLine<'a> {
    line_no: usize,
    start: usize,
    /// Line text before any `#`, trimmed.
    stripped: &'a str,
    /// Comma-separated fields of `stripped`, trimmed.
    fields: Vec<&'a str>,
}

impl HostLine<'_> {
    fn has_ip(&self, ip: Ipv4Addr) -> bool {
        self.fields
            .iter()
            .any(|field| field.parse::<Ipv4Addr>().ok() == Some(ip))
    }

    /// `mac_lc` must be lowercase. Fields compare case-insensitively.
    fn has_mac(&self, mac_lc: &str) -> bool {
        self.fields.iter().any(|f| f.to_ascii_lowercase() == mac_lc)
    }
}

/// Iterate the lines of `content` that carry data outside a comment.
fn host_lines(content: &str) -> impl Iterator<Item = HostLine<'_>> {
    iter_line_spans(content).into_iter().filter_map(|span| {
        let stripped = span.text.split('#').next().unwrap_or("").trim();
        (!stripped.is_empty()).then(|| HostLine {
            line_no: span.line_no,
            start: span.start,
            stripped,
            fields: stripped.split(',').map(str::trim).collect(),
        })
    })
}

/// Collect the data lines outside our own `# outbe-up:<block_key>:begin..end` block.
///
/// Enforces strict `# outbe-up:` marker grammar first.
fn foreign_host_lines<'a>(existing_hosts: &'a str, block_key: &str) -> Result<Vec<HostLine<'a>>> {
    let blocks = parse_managed_blocks(existing_hosts)?;
    let own_range = blocks
        .iter()
        .find(|b| b.key == block_key)
        .map(|b| b.block_start..b.block_end);
    Ok(host_lines(existing_hosts)
        .filter(|line| !own_range.as_ref().is_some_and(|r| r.contains(&line.start)))
        .collect())
}

/// Refuse when any requested bridge IP appears in a non-comment line outside our own managed
/// `# outbe-up:<block_key>:begin..end` block.
///
/// Enforces strict `# outbe-up:` marker grammar first (F2) and treats ANY foreign line carrying
/// a requested IP — even if its MAC matches ours or it lives inside another marked block — as a
/// conflict (F1). Never adopts or moves foreign lines.
pub fn check_dnsmasq_ip_conflicts(
    existing_hosts: &str,
    requested: &[(String, Ipv4Addr)],
    block_key: &str,
) -> Result<()> {
    for line in foreign_host_lines(existing_hosts, block_key)? {
        if let Some((_our_mac, our_ip)) = requested.iter().find(|(_, ip)| line.has_ip(*ip)) {
            bail!(
                "bridge IP {our_ip} conflicts with existing dnsmasq hosts line {}: `{}`",
                line.line_no,
                line.stripped
            );
        }
    }
    Ok(())
}

/// Refuse when any requested MAC already appears in ANY non-comment line outside our own managed
/// `# outbe-up:<block_key>:begin..end` block (even if mapped to the same IP — never steal or move
/// a foreign reservation) (F1).
pub fn check_dnsmasq_mac_conflicts(
    existing_hosts: &str,
    requested: &[(String, Ipv4Addr)],
    block_key: &str,
) -> Result<()> {
    for line in foreign_host_lines(existing_hosts, block_key)? {
        let conflict = requested
            .iter()
            .map(|(our_mac, _our_ip)| our_mac.to_ascii_lowercase())
            .filter(|our_mac_lc| !our_mac_lc.is_empty())
            .find(|our_mac_lc| line.has_mac(our_mac_lc));
        if let Some(our_mac_lc) = conflict {
            bail!(
                "MAC {our_mac_lc} conflicts with existing dnsmasq hosts line {}: `{}`",
                line.line_no,
                line.stripped
            );
        }
    }
    Ok(())
}

/// Return any `(mac, ip)` reservations not yet present in `existing`.
pub fn missing_dnsmasq_entries(
    existing: &str,
    expected: &[(String, Ipv4Addr)],
) -> Vec<(String, Ipv4Addr)> {
    let present: Vec<(String, Ipv4Addr)> = existing.lines().filter_map(reservation_pair).collect();
    expected
        .iter()
        .filter(|(mac, ip)| {
            let needle = (mac.to_ascii_lowercase(), *ip);
            !present.contains(&needle)
        })
        .cloned()
        .collect()
}

/// Parse the leading `mac,ip` pair of a hosts line into `(lowercase_mac, ip)`.
fn reservation_pair(line: &str) -> Option<(String, Ipv4Addr)> {
    let stripped = line.split('#').next().unwrap_or("").trim();
    let mut parts = stripped.split(',').map(str::trim);
    let mac = parts.next()?;
    let ip = parts.next()?.parse::<Ipv4Addr>().ok()?;
    Some((mac.to_ascii_lowercase(), ip))
}

/// Validate that every non-empty, non-comment line in a dnsmasq `--dhcp-hostsfile` matches
/// recognized comma-separated dnsmasq host grammar (`mac,ip[,hostname][,lease]` or tagged variants)
/// AND that all `# outbe-up:` block markers in the file obey strict balanced grammar (F2).
///
/// Returns the number of valid reservation lines parsed.
pub fn validate_dnsmasq_hosts_format(content: &str) -> Result<usize> {
    parse_managed_blocks(content)?;
    let mut count = 0usize;
    for line in host_lines(content) {
        validate_host_line(&line)?;
        count += 1;
    }
    Ok(count)
}

fn validate_host_line(line: &HostLine<'_>) -> Result<()> {
    let (line_no, stripped) = (line.line_no, line.stripped);
    let has_fields = line.fields.len() >= 2 && line.fields.iter().all(|f| !f.is_empty());
    ensure!(
        has_fields,
        "unrecognized dnsmasq hosts line {line_no} (expected comma-separated fields): `{stripped}`"
    );
    let has_mac = line.fields.iter().any(|f| is_mac_address(f));
    let has_ip = line.fields.iter().any(|f| f.parse::<Ipv4Addr>().is_ok());
    ensure!(
        has_mac || has_ip,
        "unrecognized dnsmasq hosts line {line_no} (missing MAC or IPv4 address): `{stripped}`"
    );
    let has_forbidden_chars = line.fields.iter().any(|field| {
        field
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ';')
    });
    ensure!(
        !has_forbidden_chars,
        "invalid characters in dnsmasq hosts line {line_no}: `{stripped}`"
    );
    Ok(())
}

/// Result of verifying whether one VM's `(mac, ip)` static DHCP reservation is present in a
/// dnsmasq `--dhcp-hostsfile` without any conflicting IP or MAC mappings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DnsmasqReservationCheck {
    pub exact_match: bool,
    pub conflicts: Vec<String>,
}

/// How one hosts line relates to the expected reservation.
enum ReservationLine {
    Exact,
    Conflict(String),
    Unrelated,
}

/// Check (strictly read-only) whether `existing_hosts` contains an exact static reservation for
/// `(expected_mac, expected_ip)` — either inside `# outbe-up:vm:<name>:begin..end` or as an
/// operator-entered static line — and report any conflicting lines that map `expected_ip` to a
/// different MAC or `expected_mac` to a different IP.
pub fn check_dnsmasq_reservation_status(
    existing_hosts: &str,
    expected_mac: &str,
    expected_ip: Ipv4Addr,
) -> Result<DnsmasqReservationCheck> {
    parse_managed_blocks(existing_hosts)?;
    let expected_mac_lc = expected_mac.trim().to_ascii_lowercase();
    ensure!(
        is_mac_address(&expected_mac_lc),
        "invalid expected MAC address: `{expected_mac}`"
    );

    let mut exact_match = false;
    let mut conflicts = Vec::new();
    for line in host_lines(existing_hosts) {
        match classify_reservation_line(&line, &expected_mac_lc, expected_ip) {
            ReservationLine::Exact => exact_match = true,
            ReservationLine::Conflict(conflict) => conflicts.push(conflict),
            ReservationLine::Unrelated => {}
        }
    }

    Ok(DnsmasqReservationCheck {
        exact_match: exact_match && conflicts.is_empty(),
        conflicts,
    })
}

fn classify_reservation_line(
    line: &HostLine<'_>,
    expected_mac_lc: &str,
    expected_ip: Ipv4Addr,
) -> ReservationLine {
    let (line_no, stripped) = (line.line_no, line.stripped);
    let line_macs: Vec<String> = line
        .fields
        .iter()
        .filter(|f| is_mac_address(f))
        .map(|f| f.to_ascii_lowercase())
        .collect();
    let line_ips: Vec<Ipv4Addr> = line
        .fields
        .iter()
        .filter_map(|f| f.parse::<Ipv4Addr>().ok())
        .collect();

    let has_expected_mac = line_macs.iter().any(|m| m == expected_mac_lc);
    let has_expected_ip = line_ips.contains(&expected_ip);
    let single_pair = line_macs.len() == 1 && line_ips.len() == 1;

    match (has_expected_mac, has_expected_ip) {
        (true, true) if single_pair => ReservationLine::Exact,
        (true, true) => ReservationLine::Conflict(format!(
            "ambiguous dnsmasq hosts line {line_no} contains multiple MACs or IPs: `{stripped}`"
        )),
        (false, true) => ReservationLine::Conflict(format!(
            "bridge IP {expected_ip} conflicts with existing dnsmasq hosts line {line_no}: `{stripped}`"
        )),
        (true, false) => ReservationLine::Conflict(format!(
            "MAC {expected_mac_lc} conflicts with existing dnsmasq hosts line {line_no}: `{stripped}`"
        )),
        (false, false) => ReservationLine::Unrelated,
    }
}
