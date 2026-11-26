//! Idempotent dnsmasq reservation merging, conflict checks, and host CAS command builders.

mod hosts;
mod leases;
mod remote;
#[cfg(test)]
mod tests;

use std::net::Ipv4Addr;

use eyre::{ensure, Result};

pub use hosts::{
    check_dnsmasq_ip_conflicts, check_dnsmasq_mac_conflicts, check_dnsmasq_reservation_status,
    missing_dnsmasq_entries, validate_dnsmasq_hosts_format, DnsmasqReservationCheck,
};
pub use leases::{check_dnsmasq_lease_conflicts, count_dnsmasq_leases, lookup_lease_ip_by_mac};
pub use remote::{
    build_host_reserve_write_command, build_privileged_read_command, parse_sentinel_read,
    sha256_hex,
};
pub(crate) use remote::{MISSING_SENTINEL, PRESENT_SENTINEL};

use crate::block_splice::{
    iter_line_spans, parse_managed_blocks, render_managed_block, splice_remove_block,
    splice_upsert_block_at, BlockSpliceResult,
};

pub const DNSMASQ_RESERVATIONS_FILENAME: &str = "dnsmasq-reservations.txt";
/// Finite DHCP lease duration written into managed dnsmasq reservations so
/// removed VMs do not leave permanent stale leases on shared bridges.
pub const DNSMASQ_LEASE_DURATION: &str = "10m";
pub const DEFAULT_DNSMASQ_LOCK_FILE: &str = "/run/outbe-up/dnsmasq.lock";
/// Default host dnsmasq leases file managed by `validator-vm-dhcp.service`.
pub const DEFAULT_DHCP_LEASES_FILE: &str = "/var/lib/validator-vm/dhcp-leases";

/// Format one dnsmasq `--dhcp-hostsfile` reservation line (`mac,ip,<lease>`).
pub fn dnsmasq_entry(mac: &str, ip: Ipv4Addr) -> String {
    format!(
        "{},{},{DNSMASQ_LEASE_DURATION}",
        mac.to_ascii_lowercase(),
        ip
    )
}

/// Merge our `mac,ip` entries into a shared dnsmasq hosts file via strict, byte-preserving
/// block splicing.
///
/// - Enforces strict `# outbe-up:` marker grammar across the entire file (F2).
/// - Refuses any foreign IP OR MAC conflict outside our exact managed block (F1) — never deletes,
///   moves, or steals foreign lines.
/// - Preserves every byte outside our block verbatim (CRLF, blank lines, trailing newline or its
///   absence) (F3).
pub fn merge_dnsmasq_hosts(
    existing: &str,
    our_entries: &[(String, Ipv4Addr)],
    block_key: &str,
) -> Result<String> {
    let res = merge_dnsmasq_hosts_checked(existing, our_entries, block_key)?;
    Ok(res.content)
}

/// Same as [`merge_dnsmasq_hosts`], returning full [`BlockSpliceResult`] metadata (`exact_match`,
/// `changed`, `existing_block`).
pub fn merge_dnsmasq_hosts_checked(
    existing: &str,
    our_entries: &[(String, Ipv4Addr)],
    block_key: &str,
) -> Result<BlockSpliceResult> {
    check_dnsmasq_ip_conflicts(existing, our_entries, block_key)?;
    check_dnsmasq_mac_conflicts(existing, our_entries, block_key)?;

    if our_entries.is_empty() {
        return strip_dnsmasq_block_checked(existing, block_key, None);
    }

    let body_lines: Vec<String> = our_entries
        .iter()
        .map(|(mac, ip)| dnsmasq_entry(mac, *ip))
        .collect();
    let rendered = render_managed_block(block_key, &body_lines, "")?;
    let expected_macs: Vec<String> = our_entries
        .iter()
        .map(|(mac, _)| mac.to_ascii_lowercase())
        .collect();
    let single_vm = block_key.starts_with("vm:");
    let expected_for_validation = if single_vm {
        Some(expected_macs.as_slice())
    } else {
        None
    };

    splice_upsert_block_at(existing, block_key, &rendered, existing.len(), |body| {
        validate_managed_dnsmasq_body(body, block_key, single_vm, expected_for_validation)
    })
}

/// Remove only this block's managed `# outbe-up:<block_key>:begin..end` block from a shared
/// dnsmasq hosts file, preserving every byte outside our block verbatim.
///
/// Fails closed on malformed/unclosed/nested/duplicate markers or foreign lines inside our block.
pub fn strip_dnsmasq_block_checked(
    existing: &str,
    block_key: &str,
    expected_mac: Option<&str>,
) -> Result<BlockSpliceResult> {
    let single_vm = block_key.starts_with("vm:");
    let expected_list = expected_mac.map(|m| vec![m.to_ascii_lowercase()]);
    splice_remove_block(existing, block_key, |body| {
        validate_managed_dnsmasq_body(body, block_key, single_vm, expected_list.as_deref())
    })
}

/// Legacy infallible wrapper used by the old SSH path until S6.
pub fn strip_dnsmasq_network_block(existing: &str, network_name: &str) -> String {
    strip_dnsmasq_block_checked(existing, network_name, None)
        .map(|r| r.content)
        .unwrap_or_else(|_| existing.to_owned())
}

fn validate_managed_dnsmasq_body(
    body: &str,
    block_key: &str,
    single_vm: bool,
    expected_macs: Option<&[String]>,
) -> Result<()> {
    let mut entry_count = 0usize;
    for span in iter_line_spans(body) {
        let trimmed = span.text.trim();
        ensure!(
            !trimmed.is_empty() && !trimmed.starts_with('#'),
            "refusing unexpected blank or comment line inside managed block `# outbe-up:{block_key}`: `{}`",
            span.text
        );
        ensure!(
            !trimmed.contains('#'),
            "refusing inline comment inside managed block `# outbe-up:{block_key}`: `{trimmed}`"
        );
        let fields: Vec<&str> = trimmed.split(',').map(str::trim).collect();
        let valid_shape = if single_vm {
            fields.len() == 3
                && is_mac_address(fields[0])
                && fields[1].parse::<Ipv4Addr>().is_ok()
                && fields[2] == DNSMASQ_LEASE_DURATION
        } else {
            (fields.len() == 2 || fields.len() == 3)
                && is_mac_address(fields[0])
                && fields[1].parse::<Ipv4Addr>().is_ok()
        };
        ensure!(
            valid_shape,
            "refusing unrecognized foreign line inside managed block `# outbe-up:{block_key}`: `{trimmed}`"
        );
        if let Some(allowed) = expected_macs {
            let mac_lc = fields[0].to_ascii_lowercase();
            ensure!(
                allowed.iter().any(|m| m == &mac_lc),
                "refusing foreign MAC `{mac_lc}` inside managed block `# outbe-up:{block_key}`"
            );
        }
        entry_count += 1;
    }
    if single_vm {
        ensure!(
            entry_count <= 1,
            "refusing single-VM block `# outbe-up:{block_key}` containing {entry_count} entries"
        );
    }
    Ok(())
}

/// Format the per-VM managed block (`# outbe-up:vm:<name>:begin..end`) for a single VM reservation.
pub fn render_vm_dnsmasq_block(vm_name: &str, mac: &str, ip: Ipv4Addr) -> String {
    format!(
        "# outbe-up:vm:{vm_name}:begin\n{}\n# outbe-up:vm:{vm_name}:end\n",
        dnsmasq_entry(mac, ip)
    )
}

/// Extract the exact existing `# outbe-up:<block_key>:begin..end` block (including markers), if
/// present in `existing_hosts`, after validating strict marker grammar.
pub fn extract_dnsmasq_block(existing_hosts: &str, block_key: &str) -> Result<Option<String>> {
    let blocks = parse_managed_blocks(existing_hosts)?;
    Ok(blocks
        .into_iter()
        .find(|b| b.key == block_key)
        .map(|b| existing_hosts[b.block_start..b.block_end].to_owned()))
}

fn is_mac_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}
