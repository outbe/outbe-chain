use std::{
    fs,
    time::{Duration, Instant},
};

use super::*;
use crate::shell_quote;

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
    let rm = strip_dnsmasq_block_checked(unmanaged_same, "vm:node-a", Some("52:54:00:aa:bb:88"))
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
        strip_dnsmasq_block_checked(foreign_block, "vm:node-a", Some("52:54:00:aa:bb:88")).unwrap();
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
    assert!(strip_dnsmasq_block_checked(unclosed, "vm:node-a", Some("52:54:00:aa:bb:88")).is_err());

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
    let removed_no_lf =
        strip_dnsmasq_block_checked(&added_no_lf.content, "vm:node-a", Some("52:54:00:aa:bb:88"))
            .unwrap();
    assert!(removed_no_lf.exact_match);
    assert!(removed_no_lf.changed);
    assert_eq!(removed_no_lf.content.as_bytes(), no_final_lf.as_bytes());
}

#[test]
fn check_dnsmasq_reservation_status_accepts_managed_or_operator_entries_and_flags_conflicts() {
    let managed = "\
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
# outbe-up:vm:node-a:begin
52:54:00:aa:bb:88,10.77.0.88,10m
# outbe-up:vm:node-a:end
";
    let ok_managed = check_dnsmasq_reservation_status(
        managed,
        "52:54:00:AA:BB:88",
        Ipv4Addr::new(10, 77, 0, 88),
    )
    .unwrap();
    assert!(ok_managed.exact_match);
    assert!(ok_managed.conflicts.is_empty());

    let operator_plain = "52:54:00:aa:bb:88,10.77.0.88,node-a,infinite\n";
    let ok_plain = check_dnsmasq_reservation_status(
        operator_plain,
        "52:54:00:aa:bb:88",
        Ipv4Addr::new(10, 77, 0, 88),
    )
    .unwrap();
    assert!(ok_plain.exact_match);
    assert!(ok_plain.conflicts.is_empty());

    let missing = check_dnsmasq_reservation_status(
        "be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite\n",
        "52:54:00:aa:bb:88",
        Ipv4Addr::new(10, 77, 0, 88),
    )
    .unwrap();
    assert!(!missing.exact_match);
    assert!(missing.conflicts.is_empty());

    let ip_conflict = check_dnsmasq_reservation_status(
        "be:6a:63:f8:84:d0,10.77.0.88,other,infinite\n",
        "52:54:00:aa:bb:88",
        Ipv4Addr::new(10, 77, 0, 88),
    )
    .unwrap();
    assert!(!ip_conflict.exact_match);
    assert_eq!(ip_conflict.conflicts.len(), 1);
    assert!(ip_conflict.conflicts[0].contains("10.77.0.88"));

    let mac_conflict = check_dnsmasq_reservation_status(
        "52:54:00:aa:bb:88,10.77.0.99,node-a,infinite\n",
        "52:54:00:aa:bb:88",
        Ipv4Addr::new(10, 77, 0, 88),
    )
    .unwrap();
    assert!(!mac_conflict.exact_match);
    assert_eq!(mac_conflict.conflicts.len(), 1);
    assert!(mac_conflict.conflicts[0].contains("52:54:00:aa:bb:88"));
}
