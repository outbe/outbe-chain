//! Idempotent dnsmasq reservation merging, conflict checks, and host CAS command builders.

use std::{collections::BTreeMap, net::Ipv4Addr, path::Path};

use eyre::{bail, ensure, Result};
use sha2::{Digest, Sha256};

use crate::{
    block_splice::{
        iter_line_spans, parse_managed_blocks, render_managed_block, splice_remove_block,
        splice_upsert_block_at, BlockSpliceResult,
    },
    shell_quote,
};

pub const DNSMASQ_RESERVATIONS_FILENAME: &str = "dnsmasq-reservations.txt";
/// Finite DHCP lease duration written into managed dnsmasq reservations so
/// removed VMs do not leave permanent stale leases on shared bridges.
pub const DNSMASQ_LEASE_DURATION: &str = "10m";
pub const DEFAULT_DNSMASQ_LOCK_FILE: &str = "/run/outbe-up/dnsmasq.lock";
/// Default host dnsmasq leases file managed by `validator-vm-dhcp.service`.
pub const DEFAULT_DHCP_LEASES_FILE: &str = "/var/lib/validator-vm/dhcp-leases";

pub(crate) const PRESENT_SENTINEL: &str = "OUTBE_UP_PRESENT";
pub(crate) const MISSING_SENTINEL: &str = "OUTBE_UP_MISSING";

/// Format one dnsmasq `--dhcp-hostsfile` reservation line (`mac,ip,<lease>`).
pub fn dnsmasq_entry(mac: &str, ip: Ipv4Addr) -> String {
    format!(
        "{},{},{DNSMASQ_LEASE_DURATION}",
        mac.to_ascii_lowercase(),
        ip
    )
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
    let blocks = parse_managed_blocks(existing_hosts)?;
    let own_span = blocks.iter().find(|b| b.key == block_key);

    for span in iter_line_spans(existing_hosts) {
        if own_span.is_some_and(|b| span.start >= b.block_start && span.start < b.block_end) {
            continue;
        }
        let stripped = span.text.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let fields: Vec<&str> = stripped.split(',').map(str::trim).collect();
        for (_our_mac, our_ip) in requested {
            let ip_present = fields
                .iter()
                .any(|field| field.parse::<Ipv4Addr>().ok() == Some(*our_ip));
            if ip_present {
                bail!(
                    "bridge IP {our_ip} conflicts with existing dnsmasq hosts line {}: `{stripped}`",
                    span.line_no
                );
            }
        }
    }
    Ok(())
}

/// Parse standard dnsmasq lease lines (`<expiry> <mac> <ip> <hostname> <client-id>`).
fn parse_dnsmasq_leases(existing_leases: &str) -> BTreeMap<Ipv4Addr, String> {
    let mut leases: BTreeMap<Ipv4Addr, String> = BTreeMap::new();
    for line in existing_leases.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split_whitespace().collect();
        if cols.len() >= 3 {
            if let Ok(ip) = cols[2].parse::<Ipv4Addr>() {
                leases.insert(ip, cols[1].to_ascii_lowercase());
            }
        }
    }
    leases
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
        if let Some(lease_mac) = leases.get(our_ip) {
            let our_mac_lc = our_mac.to_ascii_lowercase();
            let exact_match = !our_mac_lc.is_empty() && *lease_mac == our_mac_lc;
            if !exact_match {
                bail!(
                    "bridge IP {our_ip} is currently leased to MAC {lease_mac} in dnsmasq leases"
                );
            }
        }
    }
    Ok(())
}

/// Return any `(mac, ip)` reservations not yet present in `existing`.
pub fn missing_dnsmasq_entries(
    existing: &str,
    expected: &[(String, Ipv4Addr)],
) -> Vec<(String, Ipv4Addr)> {
    let mut present = Vec::new();
    for line in existing.lines() {
        let stripped = line.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let mut parts = stripped.split(',').map(str::trim);
        if let (Some(mac), Some(ip_str)) = (parts.next(), parts.next()) {
            if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
                present.push((mac.to_ascii_lowercase(), ip));
            }
        }
    }
    expected
        .iter()
        .filter(|(mac, ip)| {
            let needle = (mac.to_ascii_lowercase(), *ip);
            !present.contains(&needle)
        })
        .cloned()
        .collect()
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

/// Build a privileged read command that distinguishes an absent file from a read failure.
pub fn build_privileged_read_command(path: &Path) -> String {
    let quoted = shell_quote(&path.to_string_lossy());
    let inner = format!(
        "if [ -e {quoted} ]; then printf '{PRESENT_SENTINEL}\\n' && cat {quoted}; else printf '{MISSING_SENTINEL}\\n'; fi"
    );
    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -ec {script}; else sudo -n sh -ec {script}; fi",
        script = shell_quote(&inner)
    )
}

/// Parse the output of a sentinel-framed read command, failing closed on unexpected output.
pub fn parse_sentinel_read(raw: &str, path: &Path) -> Result<Option<String>> {
    if let Some(rest) = raw.strip_prefix(PRESENT_SENTINEL) {
        let body = rest.strip_prefix('\n').unwrap_or(rest);
        return Ok(Some(body.to_owned()));
    }
    if raw.trim() == MISSING_SENTINEL {
        return Ok(None);
    }
    bail!(
        "unexpected remote read response for {} (refusing to assume empty)",
        path.display()
    );
}

/// Build a locked compare-and-swap command that updates `hosts_file` from stdin and sends SIGHUP to dnsmasq.
pub fn build_host_reserve_write_command(
    hosts_file: &Path,
    pid_file: &Path,
    lock_file: &Path,
    expected_sha256: Option<&str>,
) -> String {
    let hosts_quoted = shell_quote(&hosts_file.to_string_lossy());
    let lock_quoted = shell_quote(&lock_file.to_string_lossy());
    let lock_dir_quoted = shell_quote(
        &lock_file
            .parent()
            .unwrap_or_else(|| Path::new("/run/outbe-up"))
            .to_string_lossy(),
    );
    let tmp_template = shell_quote(&format!("{}.outbe-up.XXXXXX", hosts_file.display()));
    let pid_quoted = shell_quote(&pid_file.to_string_lossy());

    let cas_check = match expected_sha256 {
        Some(expected) => {
            let expected_quoted = shell_quote(expected);
            format!(
                "if [ ! -e {hosts_quoted} ]; then echo 'hosts file disappeared during update' >&2; exit 1; fi; \
                 actual=$(sha256sum {hosts_quoted} | awk '{{print $1}}'); \
                 if [ \"$actual\" != {expected_quoted} ]; then echo 'hosts file changed concurrently; retry' >&2; exit 1; fi"
            )
        }
        None => format!(
            "if [ -e {hosts_quoted} ]; then echo 'hosts file was created concurrently; retry' >&2; exit 1; fi"
        ),
    };

    let inner = format!(
        "uid=$(id -u); gid=$(id -g); \
         if [ -L {lock_dir_quoted} ]; then echo 'lock directory must not be a symlink' >&2; exit 1; fi; \
         if [ ! -e {lock_dir_quoted} ]; then install -d -m 0700 -o \"$uid\" -g \"$gid\" {lock_dir_quoted}; fi; \
         if [ ! -d {lock_dir_quoted} ] || [ -L {lock_dir_quoted} ]; then echo 'lock directory is not a directory' >&2; exit 1; fi; \
         if [ \"$(stat -c '%u %a' {lock_dir_quoted})\" != \"$uid 700\" ]; then echo 'lock directory has unsafe owner or permissions' >&2; exit 1; fi; \
         if [ -e {lock_quoted} ] || [ -L {lock_quoted} ]; then \
           if [ ! -f {lock_quoted} ] || [ -L {lock_quoted} ] || [ \"$(stat -c '%u' {lock_quoted})\" != \"$uid\" ]; then \
             echo 'lock entry must be a regular file owned by current uid' >&2; exit 1; \
           fi; \
         fi; \
         exec 9>>{lock_quoted}; flock -x 9; \
         {cas_check}; \
         tmp=$(mktemp {tmp_template}); \
         trap 'rm -f \"$tmp\"' EXIT; \
         cat > \"$tmp\"; \
         chmod 0644 \"$tmp\"; \
         mv -f \"$tmp\" {hosts_quoted}; \
         trap - EXIT; \
         if [ -f {pid_quoted} ]; then kill -HUP \"$(cat {pid_quoted})\"; else echo 'dnsmasq pid file missing: '{pid_quoted} >&2; exit 1; fi"
    );

    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -ec {script}; else sudo -n sh -ec {script}; fi",
        script = shell_quote(&inner)
    )
}

/// Compute lowercase hex SHA-256 for CAS checks on dnsmasq hosts content.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Format the per-VM managed block (`# outbe-up:vm:<name>:begin..end`) for a single VM reservation.
pub fn render_vm_dnsmasq_block(vm_name: &str, mac: &str, ip: Ipv4Addr) -> String {
    format!(
        "# outbe-up:vm:{vm_name}:begin\n{}\n# outbe-up:vm:{vm_name}:end\n",
        dnsmasq_entry(mac, ip)
    )
}

/// Validate that every non-empty, non-comment line in a dnsmasq `--dhcp-hostsfile` matches
/// recognized comma-separated dnsmasq host grammar (`mac,ip[,hostname][,lease]` or tagged variants)
/// AND that all `# outbe-up:` block markers in the file obey strict balanced grammar (F2).
///
/// Returns the number of valid reservation lines parsed.
pub fn validate_dnsmasq_hosts_format(content: &str) -> Result<usize> {
    parse_managed_blocks(content)?;
    let mut count = 0usize;
    for span in iter_line_spans(content) {
        let stripped = span.text.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let fields: Vec<&str> = stripped.split(',').map(str::trim).collect();
        if fields.len() < 2 || fields.iter().any(|f| f.is_empty()) {
            bail!(
                "unrecognized dnsmasq hosts line {} (expected comma-separated fields): `{stripped}`",
                span.line_no
            );
        }
        let has_mac = fields.iter().any(|f| is_mac_address(f));
        let has_ip = fields.iter().any(|f| f.parse::<Ipv4Addr>().is_ok());
        if !has_mac && !has_ip {
            bail!(
                "unrecognized dnsmasq hosts line {} (missing MAC or IPv4 address): `{stripped}`",
                span.line_no
            );
        }
        for field in &fields {
            if field
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == ';')
            {
                bail!(
                    "invalid characters in dnsmasq hosts line {}: `{stripped}`",
                    span.line_no
                );
            }
        }
        count += 1;
    }
    Ok(count)
}

/// Refuse when any requested MAC already appears in ANY non-comment line outside our own managed
/// `# outbe-up:<block_key>:begin..end` block (even if mapped to the same IP — never steal or move
/// a foreign reservation) (F1).
pub fn check_dnsmasq_mac_conflicts(
    existing_hosts: &str,
    requested: &[(String, Ipv4Addr)],
    block_key: &str,
) -> Result<()> {
    let blocks = parse_managed_blocks(existing_hosts)?;
    let own_span = blocks.iter().find(|b| b.key == block_key);

    for span in iter_line_spans(existing_hosts) {
        if own_span.is_some_and(|b| span.start >= b.block_start && span.start < b.block_end) {
            continue;
        }
        let stripped = span.text.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let fields: Vec<&str> = stripped.split(',').map(str::trim).collect();
        for (our_mac, _our_ip) in requested {
            let our_mac_lc = our_mac.to_ascii_lowercase();
            if our_mac_lc.is_empty() {
                continue;
            }
            let mac_present = fields.iter().any(|f| f.to_ascii_lowercase() == our_mac_lc);
            if mac_present {
                bail!(
                    "MAC {our_mac_lc} conflicts with existing dnsmasq hosts line {}: `{stripped}`",
                    span.line_no
                );
            }
        }
    }
    Ok(())
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

/// Count non-empty, non-comment lease lines in a dnsmasq leases file.
pub fn count_dnsmasq_leases(existing_leases: &str) -> usize {
    parse_dnsmasq_leases(existing_leases).len()
}

/// Build a local locked compare-and-swap shell script that verifies file type/owner and SHA-256
/// under `flock`, atomically replaces `hosts_file` from stdin, and reloads `reload_unit` via
/// `systemctl reload` (never shelling out to `kill -HUP`).
pub fn build_local_hosts_cas_write_script(
    hosts_file: &Path,
    lock_file: &Path,
    expected_sha256: &str,
    reload_unit: &str,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<String> {
    validate_systemd_unit_name(reload_unit)?;
    let lock_preamble = build_local_lock_preamble(lock_file);
    let hosts_quoted = shell_quote(&hosts_file.to_string_lossy());
    let tmp_template = shell_quote(&format!("{}.outbe-up.XXXXXX", hosts_file.display()));
    let expected_quoted = shell_quote(expected_sha256);
    let unit_quoted = shell_quote(reload_unit);

    Ok(format!(
        "{lock_preamble} \
         if [ -L {hosts_quoted} ] || [ ! -f {hosts_quoted} ]; then echo 'dhcp-hosts file must be an existing regular file (not a symlink)' >&2; exit 1; fi; \
         if [ \"$(stat -c '%u:%g' {hosts_quoted})\" != '{expected_uid}:{expected_gid}' ]; then echo 'dhcp-hosts file owner mismatch' >&2; exit 1; fi; \
         actual=$(sha256sum {hosts_quoted} | awk '{{print $1}}'); \
         if [ \"$actual\" != {expected_quoted} ]; then echo 'hosts file changed concurrently; retry' >&2; exit 1; fi; \
         tmp=$(mktemp {tmp_template}); \
         trap 'rm -f \"$tmp\"' EXIT; \
         cat > \"$tmp\"; \
         chmod 0644 \"$tmp\"; \
         chown '{expected_uid}:{expected_gid}' \"$tmp\" 2>/dev/null || true; \
         mv -f \"$tmp\" {hosts_quoted}; \
         trap - EXIT; \
         systemctl reload {unit_quoted}"
    ))
}

/// Build a locked `systemctl reload <unit>` script used when `--apply` is re-run on an already
/// up-to-date hosts file (ensuring a previously failed reload is retried cleanly) (F5).
pub fn build_local_service_reload_script(lock_file: &Path, reload_unit: &str) -> Result<String> {
    validate_systemd_unit_name(reload_unit)?;
    let lock_preamble = build_local_lock_preamble(lock_file);
    let unit_quoted = shell_quote(reload_unit);
    Ok(format!("{lock_preamble} systemctl reload {unit_quoted}"))
}

fn build_local_lock_preamble(lock_file: &Path) -> String {
    let lock_quoted = shell_quote(&lock_file.to_string_lossy());
    let lock_dir_quoted = shell_quote(
        &lock_file
            .parent()
            .unwrap_or_else(|| Path::new("/run/outbe-up"))
            .to_string_lossy(),
    );
    format!(
        "uid=$(id -u); gid=$(id -g); \
         if [ -L {lock_dir_quoted} ]; then echo 'lock directory must not be a symlink' >&2; exit 1; fi; \
         if [ ! -e {lock_dir_quoted} ]; then install -d -m 0700 -o \"$uid\" -g \"$gid\" {lock_dir_quoted}; fi; \
         if [ ! -d {lock_dir_quoted} ] || [ -L {lock_dir_quoted} ]; then echo 'lock directory is not a directory' >&2; exit 1; fi; \
         if [ \"$(stat -c '%u %a' {lock_dir_quoted})\" != \"$uid 700\" ]; then echo 'lock directory has unsafe owner or permissions' >&2; exit 1; fi; \
         if [ -e {lock_quoted} ] || [ -L {lock_quoted} ]; then \
           if [ ! -f {lock_quoted} ] || [ -L {lock_quoted} ] || [ \"$(stat -c '%u' {lock_quoted})\" != \"$uid\" ]; then \
             echo 'lock entry must be a regular file owned by current uid' >&2; exit 1; \
           fi; \
         fi; \
         exec 9>>{lock_quoted}; flock -x 9;"
    )
}

fn validate_systemd_unit_name(unit: &str) -> Result<()> {
    if unit.is_empty()
        || !unit.ends_with(".service")
        || !unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
    {
        bail!("invalid systemd service unit name: {unit}");
    }
    Ok(())
}

fn is_mac_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn merge_dnsmasq_hosts_preserves_foreign_entries_and_is_idempotent() {
        let foreign = "\
# existing other test network
52:54:00:de:ad:01,10.77.0.2
52:54:00:de:ad:02,10.77.0.3
";
        let ours = vec![
            ("52:54:00:AA:BB:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10)),
            ("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11)),
        ];

        let merged = merge_dnsmasq_hosts(foreign, &ours, "vm-devnet").unwrap();
        assert!(merged.contains("52:54:00:de:ad:01,10.77.0.2"));
        assert!(merged.contains("52:54:00:de:ad:02,10.77.0.3"));
        assert!(merged.contains("52:54:00:aa:bb:01,10.77.0.10,10m"));
        assert!(merged.contains("52:54:00:aa:bb:02,10.77.0.11,10m"));
        assert!(missing_dnsmasq_entries(&merged, &ours).is_empty());

        // Re-running with the same entries is byte-identical.
        let merged_again = merge_dnsmasq_hosts(&merged, &ours, "vm-devnet").unwrap();
        assert_eq!(merged_again, merged);

        // Updating our entries replaces only our managed block and keeps foreign lines.
        let updated = vec![("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 20))];
        let merged_updated = merge_dnsmasq_hosts(&merged, &updated, "vm-devnet").unwrap();
        assert!(merged_updated.contains("52:54:00:de:ad:01,10.77.0.2"));
        assert!(merged_updated.contains("52:54:00:aa:bb:01,10.77.0.20,10m"));
        assert!(!merged_updated.contains("52:54:00:aa:bb:02,10.77.0.11"));

        // Stripping removes only our managed block and preserves foreign entries verbatim.
        let stripped = strip_dnsmasq_network_block(&merged_updated, "vm-devnet");
        assert_eq!(stripped, foreign);
        assert_eq!(strip_dnsmasq_network_block(&stripped, "vm-devnet"), foreign);
    }

    #[test]
    fn refuses_bridge_ip_reserved_to_foreign_mac_or_active_lease() {
        let foreign_hosts = "\
52:54:00:de:ad:01,10.77.0.11
52:54:00:de:ad:02,set:tag0,10.77.0.13
52:54:00:de:ad:03,10.77.0.14,vm-other,infinite
";
        let ours = vec![("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11))];
        let err = merge_dnsmasq_hosts(foreign_hosts, &ours, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.11"), "{err}");

        // Non-2-field dnsmasq grammar (`mac,set:tag,ip` and `mac,ip,hostname,lease`) fails closed.
        let tagged = vec![("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 13))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &tagged, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.13"), "{err}");

        let four_col = vec![("52:54:00:aa:bb:04".to_owned(), Ipv4Addr::new(10, 77, 0, 14))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &four_col, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.14"), "{err}");

        // Pre-CreateVm check (empty MAC) also refuses an occupied IP.
        let pre_create = vec![(String::new(), Ipv4Addr::new(10, 77, 0, 11))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &pre_create, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.11"), "{err}");

        // Active lease for any MAC other than the exact requested MAC+IP pair is refused.
        let leases = "0 52:54:00:fe:ed:01 10.77.0.12 other-vm *\n";
        let mismatch = vec![("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 12))];
        let err = check_dnsmasq_lease_conflicts(leases, &mismatch).unwrap_err();
        assert!(err.to_string().contains("10.77.0.12"), "{err}");
        assert!(err.to_string().contains("52:54:00:fe:ed:01"), "{err}");

        // Exact requested MAC+IP pair is allowed.
        let exact = vec![("52:54:00:fe:ed:01".to_owned(), Ipv4Addr::new(10, 77, 0, 12))];
        check_dnsmasq_lease_conflicts(leases, &exact).unwrap();
    }

    #[test]
    fn missing_dnsmasq_entries_detects_absent_or_mismatched_ips() {
        let existing = "52:54:00:aa:bb:01,10.77.0.10\n52:54:00:aa:bb:02,10.77.0.99\n";
        let expected = vec![
            ("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10)),
            ("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11)),
            ("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 12)),
        ];
        let missing = missing_dnsmasq_entries(existing, &expected);
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].1, Ipv4Addr::new(10, 77, 0, 11));
        assert_eq!(missing[1].1, Ipv4Addr::new(10, 77, 0, 12));
    }

    #[test]
    fn host_reserve_shell_commands_execute_cas_and_fail_closed() {
        use std::{
            io::Write,
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };

        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("bin");
        fs::create_dir_all(&shim_dir).unwrap();

        // Inert sudo shim strips `-n` and execs the command as the current test user.
        let sudo_shim = shim_dir.join("sudo");
        fs::write(
            &sudo_shim,
            "#!/bin/sh\nif [ \"$1\" = \"-n\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&sudo_shim, fs::Permissions::from_mode(0o755)).unwrap();

        let path_env = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let hosts = dir.path().join("dnsmasq.hosts");
        let pid = dir.path().join("dnsmasq.pid");
        let lock = dir.path().join("lock/outbe-up-dnsmasq.lock");
        let hup_marker = dir.path().join("hup.received");
        let foreign = "# other network\n52:54:00:de:ad:01,10.77.0.2\n";
        fs::write(&hosts, foreign).unwrap();

        // Spawn a real dummy process to receive the real SIGHUP from `kill -HUP` and exit promptly.
        let mut dummy_dnsmasq = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap 'echo HUP > {}; kill $spid 2>/dev/null || true; exit 0' HUP; sleep 15 & spid=$!; wait $spid",
                shell_quote(&hup_marker.to_string_lossy())
            ))
            .spawn()
            .unwrap();
        fs::write(&pid, format!("{}\n", dummy_dnsmasq.id())).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        // 1) Execute generated read command on a newline-terminated file; exact bytes match sha256sum.
        let read_cmd = build_privileged_read_command(&hosts);
        let read_out = Command::new("sh")
            .arg("-c")
            .arg(&read_cmd)
            .env("PATH", &path_env)
            .output()
            .unwrap();
        assert!(
            read_out.status.success(),
            "read failed: {}",
            String::from_utf8_lossy(&read_out.stderr)
        );
        let raw_stdout = String::from_utf8(read_out.stdout).unwrap();
        let existing = parse_sentinel_read(&raw_stdout, &hosts).unwrap().unwrap();
        assert_eq!(
            existing, foreign,
            "exact trailing newline must be preserved"
        );
        let expected_sha = sha256_hex(existing.as_bytes());

        // 2) Execute generated write command with unchanged file -> succeeds, foreign lines survive.
        let ours = vec![("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10))];
        let merged = merge_dnsmasq_hosts(&existing, &ours, "vm-devnet").unwrap();
        let write_cmd = build_host_reserve_write_command(&hosts, &pid, &lock, Some(&expected_sha));
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&write_cmd)
            .env("PATH", &path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(merged.as_bytes())
            .unwrap();
        let write_out = child.wait_with_output().unwrap();
        assert!(
            write_out.status.success(),
            "write failed: {}",
            String::from_utf8_lossy(&write_out.stderr)
        );
        let _ = dummy_dnsmasq.wait();
        let after = fs::read_to_string(&hosts).unwrap();
        assert!(
            after.starts_with(foreign),
            "foreign lines must survive byte-for-byte"
        );
        assert!(after.contains("52:54:00:aa:bb:01,10.77.0.10,10m"));
        assert_eq!(fs::read_to_string(&hup_marker).unwrap().trim(), "HUP");

        // 3) File changed between read and write -> CAS rejects and leaves file untouched.
        let concurrent = format!("{after}52:54:00:fe:ed:99,10.77.0.99\n");
        fs::write(&hosts, &concurrent).unwrap();
        let mut stale_child = Command::new("sh")
            .arg("-c")
            .arg(&write_cmd)
            .env("PATH", &path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        stale_child
            .stdin
            .take()
            .unwrap()
            .write_all(merged.as_bytes())
            .unwrap();
        let stale_out = stale_child.wait_with_output().unwrap();
        assert!(!stale_out.status.success(), "stale CAS write must fail");
        assert_eq!(
            fs::read_to_string(&hosts).unwrap(),
            concurrent,
            "failed CAS write must leave hosts file untouched"
        );

        // 4) Unreadable hosts file fails closed (skip only if test runs as root uid 0).
        if rustix::process::getuid().as_raw() != 0 {
            let unreadable = dir.path().join("unreadable.hosts");
            fs::write(&unreadable, "52:54:00:de:ad:01,10.77.0.2\n").unwrap();
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
            let unreadable_cmd = build_privileged_read_command(&unreadable);
            let unreadable_out = Command::new("sh")
                .arg("-c")
                .arg(&unreadable_cmd)
                .env("PATH", &path_env)
                .output()
                .unwrap();
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                !unreadable_out.status.success(),
                "unreadable file must fail closed"
            );
        }

        // Helper to run a write command with a 2-second timeout so a FIFO hang fails promptly.
        let run_write_bounded = |cmd: &str| -> std::process::ExitStatus {
            let mut proc = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("PATH", &path_env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let _ = proc.stdin.take().unwrap().write_all(merged.as_bytes());
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = proc.try_wait().unwrap() {
                    return status;
                }
                if Instant::now() >= deadline {
                    let _ = proc.kill();
                    let _ = proc.wait();
                    panic!("write command hung past 2s deadline (likely opened a FIFO)");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // 5) Pre-created FIFO at the lock path must fail promptly without hanging.
        let fifo_lock_dir = dir.path().join("fifo-lock");
        fs::create_dir_all(&fifo_lock_dir).unwrap();
        fs::set_permissions(&fifo_lock_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let fifo_lock = fifo_lock_dir.join("dnsmasq.lock");
        assert!(Command::new("mkfifo")
            .arg(&fifo_lock)
            .status()
            .unwrap()
            .success());
        let fifo_cmd = build_host_reserve_write_command(
            &hosts,
            &pid,
            &fifo_lock,
            Some(&sha256_hex(concurrent.as_bytes())),
        );
        assert!(
            !run_write_bounded(&fifo_cmd).success(),
            "FIFO at lock path must be refused promptly"
        );

        // 6) Foreign-type entry (directory and symlink) at the lock path must be refused.
        let dir_lock_parent = dir.path().join("dir-lock");
        fs::create_dir_all(&dir_lock_parent).unwrap();
        fs::set_permissions(&dir_lock_parent, fs::Permissions::from_mode(0o700)).unwrap();
        let dir_lock = dir_lock_parent.join("dnsmasq.lock");
        fs::create_dir_all(&dir_lock).unwrap();
        let dir_lock_cmd = build_host_reserve_write_command(
            &hosts,
            &pid,
            &dir_lock,
            Some(&sha256_hex(concurrent.as_bytes())),
        );
        assert!(
            !run_write_bounded(&dir_lock_cmd).success(),
            "directory at lock path must be refused"
        );

        // 7) Lock directory with group/other permission bits (0755) must be refused.
        let loose_lock_dir = dir.path().join("loose-lock");
        fs::create_dir_all(&loose_lock_dir).unwrap();
        fs::set_permissions(&loose_lock_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let loose_lock = loose_lock_dir.join("dnsmasq.lock");
        let loose_cmd = build_host_reserve_write_command(
            &hosts,
            &pid,
            &loose_lock,
            Some(&sha256_hex(concurrent.as_bytes())),
        );
        assert!(
            !run_write_bounded(&loose_cmd).success(),
            "lock directory with non-0700 mode must be refused"
        );
    }

    #[test]
    fn host_release_shell_command_strips_only_our_block_and_preserves_foreign() {
        use std::{
            io::Write,
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };

        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("bin");
        fs::create_dir_all(&shim_dir).unwrap();
        let sudo_shim = shim_dir.join("sudo");
        fs::write(
            &sudo_shim,
            "#!/bin/sh\nif [ \"$1\" = \"-n\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&sudo_shim, fs::Permissions::from_mode(0o755)).unwrap();
        let path_env = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let hosts = dir.path().join("dnsmasq.hosts");
        let pid = dir.path().join("dnsmasq.pid");
        let lock = dir.path().join("lock/dnsmasq.lock");
        let hup_marker = dir.path().join("hup.received");

        let foreign = "# other network\n52:54:00:de:ad:01,10.77.0.2\n";
        let ours = vec![("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10))];
        let initial = merge_dnsmasq_hosts(foreign, &ours, "vm-devnet").unwrap();
        fs::write(&hosts, &initial).unwrap();

        let mut dummy_dnsmasq = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap 'echo HUP > {}; kill $spid 2>/dev/null || true; exit 0' HUP; sleep 15 & spid=$!; wait $spid",
                shell_quote(&hup_marker.to_string_lossy())
            ))
            .spawn()
            .unwrap();
        fs::write(&pid, format!("{}\n", dummy_dnsmasq.id())).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        // Read with generated privileged read command, strip only our block, write under lock+CAS.
        let read_cmd = build_privileged_read_command(&hosts);
        let read_out = Command::new("sh")
            .arg("-c")
            .arg(&read_cmd)
            .env("PATH", &path_env)
            .output()
            .unwrap();
        assert!(read_out.status.success());
        let existing = parse_sentinel_read(&String::from_utf8(read_out.stdout).unwrap(), &hosts)
            .unwrap()
            .unwrap();
        let stripped = strip_dnsmasq_network_block(&existing, "vm-devnet");
        assert_eq!(stripped, foreign);

        let write_cmd = build_host_reserve_write_command(
            &hosts,
            &pid,
            &lock,
            Some(&sha256_hex(existing.as_bytes())),
        );
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&write_cmd)
            .env("PATH", &path_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stripped.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "host-release write failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = dummy_dnsmasq.wait();
        assert_eq!(fs::read_to_string(&hosts).unwrap(), foreign);
        assert_eq!(fs::read_to_string(&hup_marker).unwrap().trim(), "HUP");
    }

    #[test]
    fn f1_refuses_foreign_mac_or_ip_even_when_same_pair_and_never_deletes_foreign_lines() {
        let ours = vec![("52:54:00:aa:bb:88".to_owned(), Ipv4Addr::new(10, 77, 0, 88))];

        // Unmanaged line with our exact MAC+IP -> conflict (never steal or move).
        let unmanaged_same = "52:54:00:aa:bb:88,10.77.0.88,10m\n";
        assert!(merge_dnsmasq_hosts_checked(unmanaged_same, &ours, "vm:node-a").is_err());
        let rm =
            strip_dnsmasq_block_checked(unmanaged_same, "vm:node-a", Some("52:54:00:aa:bb:88"))
                .unwrap();
        assert!(!rm.exact_match);
        assert!(!rm.changed);
        assert_eq!(rm.content, unmanaged_same);

        // Foreign marked block with our exact MAC+IP -> conflict on reserve, untouched on release.
        let foreign_block = "\
# outbe-up:foreign:begin
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:foreign:end
";
        assert!(merge_dnsmasq_hosts_checked(foreign_block, &ours, "vm:node-a").is_err());
        let rm_foreign =
            strip_dnsmasq_block_checked(foreign_block, "vm:node-a", Some("52:54:00:aa:bb:88"))
                .unwrap();
        assert!(!rm_foreign.exact_match);
        assert!(!rm_foreign.changed);
        assert_eq!(rm_foreign.content, foreign_block);

        // Foreign line with our MAC and a different IP -> conflict.
        let foreign_mac = "52:54:00:aa:bb:88,10.77.0.99,infinite\n";
        let err = merge_dnsmasq_hosts_checked(foreign_mac, &ours, "vm:node-a").unwrap_err();
        assert!(err.to_string().contains("52:54:00:aa:bb:88"), "{err}");

        // Foreign line with a different MAC and our IP -> conflict.
        let foreign_ip = "52:54:00:de:ad:01,10.77.0.88,infinite\n";
        let err = merge_dnsmasq_hosts_checked(foreign_ip, &ours, "vm:node-a").unwrap_err();
        assert!(err.to_string().contains("10.77.0.88"), "{err}");
    }

    #[test]
    fn f2_refuses_malformed_markers_and_foreign_lines_inside_own_block() {
        let ours = vec![("52:54:00:aa:bb:88".to_owned(), Ipv4Addr::new(10, 77, 0, 88))];

        // Unclosed begin followed by another block -> refused everywhere.
        let unclosed = "\
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:final-amd:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
# outbe-up:final-amd:end
";
        assert!(validate_dnsmasq_hosts_format(unclosed).is_err());
        assert!(merge_dnsmasq_hosts_checked(unclosed, &ours, "vm:node-a").is_err());
        assert!(
            strip_dnsmasq_block_checked(unclosed, "vm:node-a", Some("52:54:00:aa:bb:88")).is_err()
        );

        // Foreign second line injected inside our own block -> refused on reserve and release.
        let foreign_inside = "\
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,10m
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
# outbe-up:vm:node-a:end
";
        assert!(merge_dnsmasq_hosts_checked(foreign_inside, &ours, "vm:node-a").is_err());
        assert!(strip_dnsmasq_block_checked(foreign_inside, "vm:node-a", None).is_err());

        // Comment or non-10m foreign entry inside our single-VM block -> refused.
        let comment_inside = "\
# outbe-up:vm:node-a:begin
# hand-edited comment
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:vm:node-a:end
";
        assert!(merge_dnsmasq_hosts_checked(comment_inside, &ours, "vm:node-a").is_err());
        assert!(strip_dnsmasq_block_checked(comment_inside, "vm:node-a", None).is_err());

        let wrong_lease = "\
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,infinite
# outbe-up:vm:node-a:end
";
        assert!(merge_dnsmasq_hosts_checked(wrong_lease, &ours, "vm:node-a").is_err());
        assert!(strip_dnsmasq_block_checked(wrong_lease, "vm:node-a", None).is_err());
    }

    #[test]
    fn f3_byte_preserving_dnsmasq_splice_keeps_crlf_and_blank_lines_exact() {
        let crlf_with_blanks = "02:00:00:00:00:11,10.77.0.11,infinite\r\n\r\n";
        let ours = vec![("52:54:00:aa:bb:88".to_owned(), Ipv4Addr::new(10, 77, 0, 88))];

        // Release when absent returns exact bytes with exact_match=false, changed=false.
        let absent = strip_dnsmasq_block_checked(crlf_with_blanks, "vm:node-a", None).unwrap();
        assert!(!absent.exact_match);
        assert!(!absent.changed);
        assert_eq!(absent.content.as_bytes(), crlf_with_blanks.as_bytes());

        // Reserve appends our block without altering a single byte of the CRLF + blank lines prefix.
        let added = merge_dnsmasq_hosts_checked(crlf_with_blanks, &ours, "vm:node-a").unwrap();
        assert!(added.changed);
        assert!(!added.exact_match);
        assert_eq!(
            &added.content.as_bytes()[..crlf_with_blanks.len()],
            crlf_with_blanks.as_bytes()
        );

        // Second reserve is an exact match with changed=false.
        let again = merge_dnsmasq_hosts_checked(&added.content, &ours, "vm:node-a").unwrap();
        assert!(again.exact_match);
        assert!(!again.changed);
        assert_eq!(again.content.as_bytes(), added.content.as_bytes());

        // Release restores exact original CRLF + trailing blank lines byte-for-byte.
        let removed =
            strip_dnsmasq_block_checked(&added.content, "vm:node-a", Some("52:54:00:aa:bb:88"))
                .unwrap();
        assert!(removed.exact_match);
        assert!(removed.changed);
        assert_eq!(removed.content.as_bytes(), crlf_with_blanks.as_bytes());

        // Add-then-remove on a file WITHOUT a final LF restores exact original bytes (zero extra LF).
        let no_final_lf = "02:00:00:00:00:11,10.77.0.11,infinite";
        let added_no_lf = merge_dnsmasq_hosts_checked(no_final_lf, &ours, "vm:node-a").unwrap();
        assert!(added_no_lf.changed);
        assert!(!added_no_lf.exact_match);
        let idemp_no_lf =
            merge_dnsmasq_hosts_checked(&added_no_lf.content, &ours, "vm:node-a").unwrap();
        assert!(idemp_no_lf.exact_match);
        assert!(!idemp_no_lf.changed);
        let removed_no_lf = strip_dnsmasq_block_checked(
            &added_no_lf.content,
            "vm:node-a",
            Some("52:54:00:aa:bb:88"),
        )
        .unwrap();
        assert!(removed_no_lf.exact_match);
        assert!(removed_no_lf.changed);
        assert_eq!(removed_no_lf.content.as_bytes(), no_final_lf.as_bytes());
    }
}
