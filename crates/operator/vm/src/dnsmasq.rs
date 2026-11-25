//! Idempotent dnsmasq reservation merging, conflict checks, and host CAS command builders.

use std::{collections::BTreeMap, net::Ipv4Addr, path::Path};

use eyre::{bail, Result};
use sha2::{Digest, Sha256};

use crate::shell_quote;

pub const DNSMASQ_RESERVATIONS_FILENAME: &str = "dnsmasq-reservations.txt";
/// Finite DHCP lease duration written into managed dnsmasq reservations so
/// removed VMs do not leave permanent stale leases on shared bridges.
pub const DNSMASQ_LEASE_DURATION: &str = "10m";
pub const DEFAULT_DNSMASQ_LOCK_FILE: &str = "/run/outbe-up/dnsmasq.lock";

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

/// Refuse when any requested bridge IP appears in a non-comment line outside our managed block.
///
/// Fails closed on any dnsmasq hosts grammar (`mac,ip`, `mac,set:tag,ip`, `mac,ip,host,lease`,
/// etc.): any line outside our managed block containing a requested IP as a comma-separated field
/// is a conflict unless it is an exact match for `our_mac` (when `our_mac` is non-empty).
pub fn check_dnsmasq_ip_conflicts(
    existing_hosts: &str,
    requested: &[(String, Ipv4Addr)],
    network_name: &str,
) -> Result<()> {
    let begin = format!("# outbe-up:{network_name}:begin");
    let end = format!("# outbe-up:{network_name}:end");
    let mut inside_ours = false;

    for (line_no, line) in existing_hosts.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == begin {
            inside_ours = true;
            continue;
        }
        if trimmed == end {
            inside_ours = false;
            continue;
        }
        if inside_ours {
            continue;
        }
        let stripped = line.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let fields: Vec<&str> = stripped.split(',').map(str::trim).collect();
        let first_field_lc = fields.first().map(|f| f.to_ascii_lowercase());
        for (our_mac, our_ip) in requested {
            let ip_present = fields
                .iter()
                .any(|field| field.parse::<Ipv4Addr>().ok() == Some(*our_ip));
            if !ip_present {
                continue;
            }
            let our_mac_lc = our_mac.to_ascii_lowercase();
            let exact_own_line = !our_mac_lc.is_empty()
                && (fields.len() == 2 || fields.len() == 3)
                && first_field_lc.as_deref() == Some(our_mac_lc.as_str())
                && fields
                    .get(1)
                    .and_then(|field| field.parse::<Ipv4Addr>().ok())
                    == Some(*our_ip);
            if !exact_own_line {
                bail!(
                    "bridge IP {our_ip} conflicts with existing dnsmasq hosts line {}: `{stripped}`",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

/// Refuse when any requested bridge IP has an active lease for a MAC other than the exact
/// requested MAC+IP pair.
///
/// Standard dnsmasq lease format: `<expiry> <mac> <ip> <hostname> <client-id>`.
pub fn check_dnsmasq_lease_conflicts(
    existing_leases: &str,
    requested: &[(String, Ipv4Addr)],
) -> Result<()> {
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

/// Merge our network's `mac,ip` entries into a shared dnsmasq hosts file idempotently,
/// refusing any IP conflict with foreign reservations.
///
/// Foreign lines outside our managed block are preserved verbatim so co-located test
/// networks on the same VMM host are never disturbed.
pub fn merge_dnsmasq_hosts(
    existing: &str,
    our_entries: &[(String, Ipv4Addr)],
    network_name: &str,
) -> Result<String> {
    check_dnsmasq_ip_conflicts(existing, our_entries, network_name)?;

    let begin = format!("# outbe-up:{network_name}:begin");
    let end = format!("# outbe-up:{network_name}:end");
    let our_macs: Vec<String> = our_entries
        .iter()
        .map(|(mac, _)| mac.to_ascii_lowercase())
        .collect();

    let mut kept = Vec::new();
    let mut inside_ours = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == begin {
            inside_ours = true;
            continue;
        }
        if trimmed == end {
            inside_ours = false;
            continue;
        }
        if inside_ours {
            continue;
        }
        // Drop any pre-existing unmanaged line for one of our own MACs to avoid duplicate leases.
        let stripped = line.split('#').next().unwrap_or("").trim();
        if let Some((mac, _)) = stripped.split_once(',') {
            if our_macs
                .iter()
                .any(|ours| ours == &mac.trim().to_ascii_lowercase())
            {
                continue;
            }
        }
        kept.push(line.to_owned());
    }

    // Trim trailing blank lines before appending our managed block.
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }

    if !our_entries.is_empty() {
        if !kept.is_empty() {
            kept.push(String::new());
        }
        kept.push(begin);
        for (mac, ip) in our_entries {
            kept.push(dnsmasq_entry(mac, *ip));
        }
        kept.push(end);
    }

    if kept.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", kept.join("\n")))
    }
}

/// Remove only this network's managed `# outbe-up:<network>:begin..end` block from a shared
/// dnsmasq hosts file, preserving all foreign entries verbatim.
pub fn strip_dnsmasq_network_block(existing: &str, network_name: &str) -> String {
    let begin = format!("# outbe-up:{network_name}:begin");
    let end = format!("# outbe-up:{network_name}:end");
    let mut kept = Vec::new();
    let mut inside_ours = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == begin {
            inside_ours = true;
            continue;
        }
        if trimmed == end {
            inside_ours = false;
            continue;
        }
        if inside_ours {
            continue;
        }
        kept.push(line.to_owned());
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
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
}
