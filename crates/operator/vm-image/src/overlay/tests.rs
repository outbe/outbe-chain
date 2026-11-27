use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use super::*;
use crate::repro::hash_directory_tree;

#[test]
fn overlay_assembly_stages_binaries_units_libs_and_keeps_ssh_dev_only() {
    let temp = tempfile::tempdir().unwrap();
    let dstack_root = temp.path().join("dstack");
    let software_root = temp.path().join("software");
    let libs_dir = temp.path().join("libs");
    fs::create_dir_all(software_root.join("bin")).unwrap();
    fs::create_dir_all(software_root.join(".bb-crs")).unwrap();
    fs::create_dir_all(&libs_dir).unwrap();

    for binary in VM_BINARIES {
        let path = software_root.join("bin").join(binary);
        fs::write(&path, format!("elf-{binary}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Host-only operator CLIs exist in the bundle/workspace but must NEVER enter the VM image.
    for excluded in ["outbe-up", "outbe-net-up"] {
        let path = software_root.join("bin").join(excluded);
        fs::write(&path, format!("elf-{excluded}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // CRS exists in the software bundle, but must NOT be copied into the VM image.
    fs::write(software_root.join(".bb-crs/bn254_g1.dat"), b"crs").unwrap();

    for lib in PINNED_RUNTIME_LIBS {
        fs::write(libs_dir.join(lib.name), format!("so-{}", lib.name)).unwrap();
    }

    stage_overlay(&dstack_root, &software_root, &libs_dir).unwrap();

    let common = dstack_root.join("os/mkosi/mkosi.skeleton");
    let dev_only = dstack_root.join("os/mkosi/mkosi.profiles/dev/mkosi.skeleton");

    // All 7 VM binaries installed with 0755 (including outbe-vm-agent); outbe-up,
    // outbe-net-up, and CRS omitted.
    for binary in VM_BINARIES {
        let dst = common.join("usr/local/bin").join(binary);
        assert!(dst.is_file(), "missing binary: {binary}");
        assert_eq!(
            fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    assert!(common.join("usr/local/bin/outbe-vm-agent").is_file());
    assert!(!common.join("usr/local/bin/outbe-up").exists());
    assert!(!common.join("usr/local/bin/outbe-net-up").exists());
    assert!(!common.join("opt/outbe-chain/.bb-crs").exists());
    assert!(!common.join(".bb-crs").exists());

    // Runtime libs and ld.so.conf.d present.
    for lib in PINNED_RUNTIME_LIBS {
        assert!(common
            .join("usr/local/lib/outbe/lib")
            .join(lib.name)
            .is_file());
    }
    assert_eq!(
        fs::read_to_string(common.join("etc/ld.so.conf.d/outbe.conf")).unwrap(),
        "/usr/local/lib/outbe/lib\n"
    );

    // System user, mountpoint, preset with `dstack` in name, all VM units + data mount unit.
    let sysusers = fs::read_to_string(common.join("usr/lib/sysusers.d/outbe.conf")).unwrap();
    assert!(sysusers.contains("u outbe 900:900"));
    assert!(sysusers.contains("/opt/outbe-chain"));
    assert!(common.join("opt/outbe-chain").is_dir());
    let wired = fs::read_to_string(common.join("etc/systemd/network/20-wired.network")).unwrap();
    assert!(wired.contains("DHCP=no"));
    assert!(wired.contains("RequiredForOnline=no"));
    assert!(wired.contains("ActivationPolicy=up"));
    assert!(wired.contains("IPv6AcceptRA=no"));
    assert!(wired.contains("LinkLocalAddressing=no"));
    let net_preset = fs::read_to_string(
        common.join("usr/lib/systemd/system-preset/75-dstack-outbe-network.preset"),
    )
    .unwrap();
    assert_eq!(net_preset, "disable systemd-networkd-wait-online.service\n");
    assert_eq!(
        fs::read_link(common.join("etc/systemd/system/systemd-networkd-wait-online.service"))
            .unwrap(),
        Path::new("/dev/null")
    );
    assert!(common
        .join("usr/lib/systemd/system-preset/85-dstack-outbe.preset")
        .is_file());

    let systemd = common.join("usr/lib/systemd/system");
    for unit in vm_units() {
        assert!(
            systemd.join(&unit.name).is_file(),
            "missing unit {}",
            unit.name
        );
    }
    let mount_unit = fs::read_to_string(systemd.join("outbe-data-mount.service")).unwrap();
    assert!(mount_unit.contains("mount --bind /dstack/persistent/outbe-chain /opt/outbe-chain"));
    assert!(mount_unit.contains("remount,bind,noexec,nosuid,nodev /opt/outbe-chain"));

    // SSH, diagnostic timer/script, and service-IP metrics drop-ins are strictly in the dev
    // profile skeleton, NEVER in common/prod.
    assert!(!common.join("etc/ssh").exists());
    assert!(!common.join("etc/systemd/journald.conf.d").exists());
    assert!(!common.join("usr/local/bin/outbe-vm-diag.sh").exists());
    assert!(!common.join("usr/lib/systemd/system/ssh.service.d").exists());
    assert!(!common
        .join("usr/lib/systemd/system/outbe-vm-diag.service")
        .exists());
    assert!(!common
        .join("usr/lib/systemd/system/outbe-vm-diag.timer")
        .exists());
    assert!(!common
        .join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset")
        .exists());
    for dropin in vm_dev_dropins() {
        assert!(
            !common
                .join("usr/lib/systemd/system")
                .join(&dropin.name)
                .exists(),
            "prod/common overlay must not contain dev drop-in {}",
            dropin.name
        );
    }
    // Pin exact file tree in common/prod overlay: no stray dev diagnostics or overrides.
    let common_files = hash_directory_tree(&common).unwrap();
    for rel in common_files.keys() {
        assert!(!rel.contains("diag"), "prod overlay must not contain {rel}");
        assert!(
            !rel.contains("journald"),
            "prod overlay must not contain {rel}"
        );
        assert!(
            !rel.contains("outbe-dev"),
            "prod overlay must not contain {rel}"
        );
        assert!(
            !rel.contains(".service.d/"),
            "prod overlay must not contain service drop-in {rel}"
        );
    }
    // Prod node units stay pinned on loopback 127.0.0.1:9101 metrics bind.
    for node_unit in [
        "outbe-validator.service",
        "outbe-joining-validator.service",
        "outbe-full-node.service",
    ] {
        let content = fs::read_to_string(systemd.join(node_unit)).unwrap();
        assert!(content.contains("  --metrics 127.0.0.1:9101 \\"));
        assert!(!content.contains("--metrics ${OUTBE_VM_IP}:9101"));
        assert!(!content.contains("--log.stdout.filter"));
        assert!(!content.contains("StandardOutput=journal+console"));
    }

    // Dev-only SSH configuration.
    let dev_sshd =
        fs::read_to_string(dev_only.join("etc/ssh/sshd_config.d/00-outbe-dev.conf")).unwrap();
    assert!(dev_sshd.contains("PubkeyAuthentication yes"));
    assert!(dev_sshd.contains("PermitRootLogin prohibit-password"));
    assert!(!dev_sshd.contains("StrictModes"));
    assert!(dev_sshd.contains("AuthorizedKeysFile /opt/outbe-chain/dev/authorized_keys"));
    let dev_ssh_dropin =
        fs::read_to_string(dev_only.join("usr/lib/systemd/system/ssh.service.d/20-outbe-dev.conf"))
            .unwrap();
    assert!(dev_ssh_dropin.contains("After=outbe-data-mount.service"));
    assert!(dev_ssh_dropin.contains("Wants=outbe-data-mount.service"));

    // The dev image does not forward the journal to the serial console.
    assert!(!dev_only.join("etc/systemd/journald.conf.d").exists());

    // Dev-only unit drop-ins: service-IP metrics on node units, no console output.
    let dev_systemd = dev_only.join("usr/lib/systemd/system");
    for dropin in vm_dev_dropins() {
        let path = dev_systemd.join(&dropin.name);
        assert!(path.is_file(), "missing dev drop-in {}", dropin.name);
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("console"),
            "{}: dev drop-in must not write to the console",
            dropin.name
        );
    }
    for service in [
        "dstack-guest-agent.service",
        "outbe-feeder.service",
        "outbe-hyperlane-validator.service",
        "outbe-radicle.service",
    ] {
        assert!(
            !dev_systemd.join(format!("{service}.d")).exists(),
            "{service}: dev image must not carry a console drop-in"
        );
    }
    for node_unit in [
        "outbe-validator.service",
        "outbe-joining-validator.service",
        "outbe-full-node.service",
    ] {
        let dropin =
            fs::read_to_string(dev_systemd.join(format!("{node_unit}.d/20-outbe-dev.conf")))
                .unwrap();
        assert!(dropin.contains("--metrics ${OUTBE_VM_IP}:9101"));
        assert!(dropin.contains("--log.stdout.filter info,commonware_p2p=debug"));
        assert!(!dropin.contains("127.0.0.1:9101"));
    }

    // Dev-only 60s diagnostic timer, service, script (0755), and preset.
    let diag_script = dev_only.join("usr/local/bin/outbe-vm-diag.sh");
    assert!(diag_script.is_file());
    assert_eq!(
        fs::metadata(&diag_script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let diag_service = fs::read_to_string(dev_systemd.join("outbe-vm-diag.service")).unwrap();
    assert!(diag_service.contains("EnvironmentFile=-/opt/outbe-chain/validator.env"));
    assert!(diag_service.contains("ExecStart=/usr/local/bin/outbe-vm-diag.sh"));
    assert!(diag_service.contains("StandardOutput=journal\n"));
    assert!(!diag_service.contains("console"));
    let diag_timer = fs::read_to_string(dev_systemd.join("outbe-vm-diag.timer")).unwrap();
    assert!(diag_timer.contains("OnBootSec=60s"));
    assert!(diag_timer.contains("OnUnitActiveSec=60s"));
    assert!(diag_timer.contains("Unit=outbe-vm-diag.service"));
    assert!(diag_timer.contains("WantedBy=timers.target"));

    let dev_preset = fs::read_to_string(
        dev_only.join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset"),
    )
    .unwrap();
    assert_eq!(
        dev_preset,
        "enable ssh.service\nenable outbe-vm-diag.timer\n"
    );
}

#[test]
fn dev_diag_script_passes_bash_syntax_and_emits_tagged_block() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("outbe-vm-diag.sh");
    write_overlay_file(&script_path, &render_dev_diag_script(), 0o755).unwrap();

    // 1. Bash syntax check.
    let syntax = Command::new("bash")
        .arg("-n")
        .arg(&script_path)
        .output()
        .unwrap();
    assert!(
        syntax.status.success(),
        "bash -n failed: {}",
        String::from_utf8_lossy(&syntax.stderr)
    );

    // 2. Functional execution against our own PID and a local mock metrics endpoint.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = concat!(
                    "# TYPE reth_process_resident_memory_bytes gauge\n",
                    "reth_process_resident_memory_bytes 123456789\n",
                    "reth_sync_caching_storage_cache_size 4200\n",
                    "reth_sync_caching_storage_cache_capacity 16777216\n",
                    "reth_txpool_cache_size 99\n",
                    "jemalloc_allocated_bytes 654321\n",
                    "reth_unrelated_metric 7\n",
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });

    let boot_marker = temp.path().join("boot-marker");
    let self_pid = std::process::id().to_string();
    let metrics_url = format!("http://127.0.0.1:{port}/metrics");

    // The first run hides `ss` and the second run uses a stub `ss`, so the `ss:` section does not
    // depend on the host tools or its open sockets.
    let path_no_ss = path_without_tool("ss", &temp.path().join("path-no-ss"));
    let path_stub_ss = path_with_stub_ss(&temp.path().join("path-stub-ss"));

    let run_once = |path: &OsStr| {
        let out = Command::new(&script_path)
            .env("PATH", path)
            .env("OUTBE_DIAG_PID", &self_pid)
            .env("OUTBE_DIAG_METRICS_URL", &metrics_url)
            .env("OUTBE_DIAG_BOOT_MARKER", &boot_marker)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "diag script failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    let first = run_once(&path_no_ss);
    let second = run_once(&path_stub_ss);
    server.join().unwrap();

    // Every non-empty line must be prefixed with `OUTBE-DIAG <unix_ts> `.
    let lines: Vec<&str> = first.lines().collect();
    assert!(
        lines.len() >= 10,
        "unexpectedly short diag output:\n{first}"
    );
    for line in &lines {
        let mut parts = line.splitn(3, ' ');
        assert_eq!(parts.next(), Some("OUTBE-DIAG"), "bad prefix: {line}");
        let ts = parts.next().expect("missing unix_ts");
        assert!(
            !ts.is_empty() && ts.bytes().all(|b| b.is_ascii_digit()),
            "non-numeric unix_ts in: {line}"
        );
    }

    // First run logs THP once at boot; second run does not repeat it.
    assert!(first.contains("thp.enabled:"), "{first}");
    assert!(!second.contains("thp.enabled:"), "{second}");

    // Required sections present.
    assert!(first.contains("loadavg:"), "{first}");
    assert!(first.contains("meminfo:"), "{first}");
    assert!(first.contains("MemTotal="), "{first}");
    assert!(
        first.contains(&format!("proc[{self_pid}].status:")),
        "{first}"
    );
    assert!(first.contains("VmRSS="), "{first}");
    assert!(first.contains("RssAnon="), "{first}");
    assert!(first.contains("RssFile="), "{first}");
    assert!(first.contains("Threads="), "{first}");
    assert!(
        first.contains(&format!("proc[{self_pid}].smaps_rollup:")),
        "{first}"
    );
    assert!(
        first.contains(&format!("proc[{self_pid}].smaps_64m_mappings: count=")),
        "{first}"
    );
    assert!(
        first.contains(&format!("proc[{self_pid}].smaps_anon_top: range=")),
        "{first}"
    );
    assert!(
        first.contains(&format!("proc[{self_pid}].threads_by_comm:")),
        "{first}"
    );
    assert!(
        first.contains(&format!("proc[{self_pid}].threads_top: tid=")),
        "{first}"
    );
    assert!(first.contains("ss: unavailable"), "{first}");
    assert!(
        second.contains("ss: ESTAB 0 0 10.0.0.1:30400 10.0.0.2:5555"),
        "{second}"
    );
    assert!(
        first.contains("metrics: reth_process_resident_memory_bytes 123456789"),
        "{first}"
    );
    assert!(
        first.contains("metrics: reth_sync_caching_storage_cache_size 4200"),
        "{first}"
    );
    assert!(
        first.contains("metrics: reth_txpool_cache_size 99"),
        "{first}"
    );
    assert!(
        first.contains("metrics: jemalloc_allocated_bytes 654321"),
        "{first}"
    );
    assert!(!first.contains("reth_unrelated_metric"), "{first}");
    assert!(!first.contains("# TYPE"), "{first}");
}

/// Return a PATH that mirrors the current PATH with `tool` hidden. Each PATH directory that holds
/// `tool` is replaced by a directory of symlinks to its other entries.
fn path_without_tool(tool: &str, mirror_root: &Path) -> OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&original)
        .enumerate()
        .map(|(idx, dir)| {
            if fs::symlink_metadata(dir.join(tool)).is_err() {
                return dir;
            }
            let mirror = mirror_root.join(idx.to_string());
            fs::create_dir_all(&mirror).unwrap();
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                if entry.file_name() != tool {
                    std::os::unix::fs::symlink(entry.path(), mirror.join(entry.file_name()))
                        .unwrap();
                }
            }
            mirror
        })
        .collect();
    std::env::join_paths(dirs).unwrap()
}

/// Return the current PATH with a stub `ss` in front. The stub prints one `outbe-chain` socket.
fn path_with_stub_ss(stub_dir: &Path) -> OsString {
    fs::create_dir_all(stub_dir).unwrap();
    write_overlay_file(
        &stub_dir.join("ss"),
        "#!/bin/sh\necho 'ESTAB 0      0   10.0.0.1:30400   10.0.0.2:5555 users:((\"outbe-chain\",pid=1,fd=3))'\n",
        0o755,
    )
    .unwrap();
    let original = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(stub_dir.to_path_buf()).chain(std::env::split_paths(&original));
    std::env::join_paths(dirs).unwrap()
}
