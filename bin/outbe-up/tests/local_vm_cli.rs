//! Executing CLI integration tests running the compiled `outbe-up` binary against a fake
//! local VMM Unix socket server across create -> start -> status -> logs -> stop -> remove,
//! plus F6 (`stop --force` on exited), F7 (unsafe permissions + `--fix-permissions`), and
//! F8 (`O_CLOEXEC` lock fd non-inheritance + `SIGTERM` cancellation both while waiting on the
//! per-VM lock and during `qemu-img`).

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440999";
const TEST_MAC: &str = "52:54:00:ab:cd:ef";

struct FakeVm {
    name: String,
    image: String,
    vcpu: u32,
    memory: u32,
    app_id: String,
    status: String,
    running: bool,
    stopped_flag: bool,
    annotations: BTreeMap<String, String>,
    remove_polls_left: usize,
}

struct FakeServer {
    sock: PathBuf,
    stop: Arc<AtomicBool>,
    rpc_count: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start(sock: PathBuf) -> Self {
        fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let vms: Arc<Mutex<BTreeMap<String, FakeVm>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let rpc_count = Arc::new(AtomicUsize::new(0));
        let stop_bg = Arc::clone(&stop);
        let rpc_bg = Arc::clone(&rpc_count);
        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        rpc_bg.fetch_add(1, Ordering::SeqCst);
                        serve_one(&mut stream, &vms);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            sock,
            stop,
            rpc_count,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_file(&self.sock);
    }
}

fn serve_one(
    stream: &mut std::os::unix::net::UnixStream,
    vms: &Arc<Mutex<BTreeMap<String, FakeVm>>>,
) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let mut header_end = None;
    let mut content_len = 0usize;
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if header_end.is_none() {
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&raw[..pos]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
            }
        }
        if let Some(end) = header_end {
            if raw.len() >= end + content_len {
                break;
            }
        }
    }
    let Some(end) = header_end else { return };
    let head = String::from_utf8_lossy(&raw[..end]);
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    let method = target
        .strip_prefix("/prpc/")
        .and_then(|rest| rest.split('?').next())
        .unwrap_or("");
    let req: Value = serde_json::from_slice(&raw[end..end + content_len]).unwrap_or(json!({}));

    let mut guard = vms.lock().unwrap();
    let body = match method {
        "Status" => {
            let keyword = req["keyword"].as_str().unwrap_or("");
            let brief = req["brief"].as_bool().unwrap_or(false);
            let vms_json: Vec<Value> = guard
                .iter()
                .filter(|(_, vm)| keyword.is_empty() || vm.name.contains(keyword))
                .map(|(id, vm)| {
                    let config = if brief {
                        Value::Null
                    } else {
                        json!({
                            "image": vm.image,
                            "vcpu": vm.vcpu,
                            "memory": vm.memory,
                            "app_id": vm.app_id,
                            "stopped": vm.stopped_flag,
                            "annotations": vm.annotations,
                        })
                    };
                    json!({
                        "id": id,
                        "name": vm.name,
                        "status": vm.status,
                        "app_id": vm.app_id,
                        "running": vm.running,
                        "configuration": config,
                        "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                    })
                })
                .collect();
            let total = vms_json.len();
            json!({ "vms": vms_json, "total": total }).to_string()
        }
        "CreateVm" => {
            let annotations: BTreeMap<String, String> = req["annotations"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            guard.insert(
                TEST_UUID.into(),
                FakeVm {
                    name: req["name"].as_str().unwrap().into(),
                    image: req["image"].as_str().unwrap().into(),
                    vcpu: req["vcpu"].as_u64().unwrap() as u32,
                    memory: req["memory"].as_u64().unwrap() as u32,
                    app_id: req["app_id"].as_str().unwrap_or("").into(),
                    status: "stopped".into(),
                    running: false,
                    stopped_flag: true,
                    annotations,
                    remove_polls_left: 1,
                },
            );
            json!({ "id": TEST_UUID }).to_string()
        }
        "GetInfo" => {
            let id = req["id"].as_str().unwrap_or("");
            let mut gone = false;
            let resp = match guard.get_mut(id) {
                Some(vm) if vm.status == "removing" => {
                    if vm.remove_polls_left == 0 {
                        gone = true;
                        json!({ "found": false, "info": null })
                    } else {
                        vm.remove_polls_left -= 1;
                        json!({
                            "found": true,
                            "info": {
                                "id": id,
                                "name": vm.name,
                                "status": "removing",
                                "app_id": vm.app_id,
                                "running": false,
                                "uptime": "",
                                "configuration": {
                                    "image": vm.image,
                                    "vcpu": vm.vcpu,
                                    "memory": vm.memory,
                                    "app_id": vm.app_id,
                                    "stopped": vm.stopped_flag,
                                    "annotations": vm.annotations,
                                },
                                "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                            }
                        })
                    }
                }
                Some(vm) => json!({
                    "found": true,
                    "info": {
                        "id": id,
                        "name": vm.name,
                        "status": vm.status,
                        "app_id": vm.app_id,
                        "running": vm.running,
                        "uptime": if vm.running { "2m 0s" } else { "" },
                        "configuration": {
                            "image": vm.image,
                            "vcpu": vm.vcpu,
                            "memory": vm.memory,
                            "app_id": vm.app_id,
                            "stopped": vm.stopped_flag,
                            "annotations": vm.annotations,
                        },
                        "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                    }
                }),
                None => json!({ "found": false, "info": null }),
            };
            if gone {
                guard.remove(id);
            }
            resp.to_string()
        }
        "StartVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = true;
            vm.stopped_flag = false;
            vm.status = "running".into();
            "{}".into()
        }
        "ShutdownVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.status = "exited".into();
            "{}".into()
        }
        "StopVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.stopped_flag = true;
            vm.status = "stopped".into();
            "{}".into()
        }
        "RemoveVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.status = "removing".into();
            "{}".into()
        }
        _ => "{}".into(),
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

#[test]
fn outbe_up_cli_executes_local_vm_flow_with_json_output() {
    let has_qemu_img = Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_qemu_img {
        eprintln!("SKIPPED: qemu-img not available in PATH");
        return;
    }

    let bin = env!("CARGO_BIN_EXE_outbe-up");
    let dir = tempfile::tempdir().unwrap();
    let vmm_home = dir.path().join("vmm");
    let sock = vmm_home.join("run/vmm.sock");
    let state_dir = dir.path().join("state");
    let leases = dir.path().join("dhcp-leases");
    fs::write(&leases, format!("1795598734 {TEST_MAC} 10.77.0.91 dev *\n")).unwrap();

    let image_dir = dir.path().join("img");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "1122334455667788".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();

    let raw_disk = dir.path().join("dev.img");
    fs::File::create(&raw_disk)
        .unwrap()
        .set_len(16 * 1024 * 1024)
        .unwrap();

    let _server = FakeServer::start(sock.clone());

    let run_cli = |args: &[&str]| {
        Command::new(bin)
            .args(args)
            .output()
            .expect("spawn outbe-up")
    };

    // 1. create --json
    let out = run_cli(&[
        "vm",
        "create",
        "--name",
        "dev",
        "--image",
        image_dir.to_str().unwrap(),
        "--disk",
        raw_disk.to_str().unwrap(),
        "--bridge",
        "obr0",
        "--ip",
        "10.77.0.88",
        "--disk-gib",
        "1",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        out.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let created: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(created["vm_id"], json!(TEST_UUID));
    assert_eq!(created["mac"], json!(TEST_MAC));

    // 2. start --json
    let out = run_cli(&[
        "vm",
        "start",
        "--name",
        "dev",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        out.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(started["vmm_status"], json!("running"));

    // 3. F7: unsafe state-dir permissions refused without --fix-permissions, repaired with it.
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o777)).unwrap();
    let bad_perm = run_cli(&[
        "vm",
        "status",
        "--name",
        "dev",
        "--dhcp-leases",
        leases.to_str().unwrap(),
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(!bad_perm.status.success());
    assert!(String::from_utf8_lossy(&bad_perm.stderr).contains("unsafe mode"));

    let out = run_cli(&[
        "vm",
        "status",
        "--name",
        "dev",
        "--dhcp-leases",
        leases.to_str().unwrap(),
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--fix-permissions",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "status --fix-permissions failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["service_ip"], json!("10.77.0.88"));
    assert_eq!(status["dhcp_ip"], json!("10.77.0.91"));
    assert_eq!(status["ip"], json!("10.77.0.88"));
    assert_eq!(status["intended_ip"], json!("10.77.0.88"));
    assert!(status["disk_source_sha256"]
        .as_str()
        .is_some_and(|s| s.len() == 64));
    assert_eq!(status["uptime"], json!("2m 0s"));
    assert_eq!(status["image_name"], json!("outbe-vm-dev-1122334455667788"));

    // Verify human table displays both SERVICE_IP and DHCP_IP.
    let table_out = run_cli(&[
        "vm",
        "status",
        "--name",
        "dev",
        "--dhcp-leases",
        leases.to_str().unwrap(),
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
    ]);
    assert!(table_out.status.success());
    let table_str = String::from_utf8_lossy(&table_out.stdout);
    assert!(table_str.contains("SERVICE_IP"), "{table_str}");
    assert!(table_str.contains("DHCP_IP"), "{table_str}");
    assert!(table_str.contains("10.77.0.88"), "{table_str}");
    assert!(table_str.contains("10.77.0.91"), "{table_str}");

    // 4. logs --json
    fs::write(
        vmm_home.join(format!("vm/{TEST_UUID}/serial.log")),
        "boot-a\nboot-b\nboot-c\n",
    )
    .unwrap();
    let out = run_cli(&[
        "vm",
        "logs",
        "--name",
        "dev",
        "--lines",
        "2",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success());
    let logs: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(logs["log"], json!("boot-b\nboot-c\n"));

    // 5. stop --json (graceful -> exited)
    let out = run_cli(&[
        "vm",
        "stop",
        "--name",
        "dev",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success());
    let stopped: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(stopped["forced"], json!(false));
    assert_eq!(stopped["vmm_status"], json!("exited"));

    // 6. F6: stop --force on `exited` VM sends StopVm to clear dstack `.started` flag -> stopped.
    let out = run_cli(&[
        "vm",
        "stop",
        "--name",
        "dev",
        "--force",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("12-defect-pending-dkg-snapshot.md"));
    let cleared: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cleared["forced"], json!(true));
    assert_eq!(cleared["cleared_started_flag"], json!(true));
    assert_eq!(cleared["vmm_status"], json!("stopped"));

    // 7. remove --json
    let out = run_cli(&[
        "vm",
        "remove",
        "--name",
        "dev",
        "--vmm-socket",
        sock.to_str().unwrap(),
        "--state-dir",
        state_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        out.status.success(),
        "remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let removed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(removed["vm_id"], json!(TEST_UUID));
    assert!(!state_dir.join("vms/dev.json").exists());

    // 8. Legacy multi-founder `--artifacts` flag has been removed from `outbe-up vm`.
    let legacy_out = run_cli(&["vm", "stop", "--artifacts", dir.path().to_str().unwrap()]);
    assert!(!legacy_out.status.success());
    assert!(
        String::from_utf8_lossy(&legacy_out.stderr).contains("--artifacts"),
        "expected unknown argument error for --artifacts: {}",
        String::from_utf8_lossy(&legacy_out.stderr)
    );
}

#[test]
fn f8_sigterm_while_waiting_for_vm_lock_exits_promptly_without_any_vmm_rpc() {
    let bin = env!("CARGO_BIN_EXE_outbe-up");
    let dir = tempfile::tempdir().unwrap();
    let vmm_home = dir.path().join("vmm");
    let sock = vmm_home.join("run/vmm.sock");
    let state_dir = dir.path().join("state");

    let image_dir = dir.path().join("img");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "aabbccddeeff0011".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();

    let raw_disk = dir.path().join("dev.img");
    fs::write(&raw_disk, b"raw-disk").unwrap();

    let server = FakeServer::start(sock.clone());
    let store = outbe_operator_vm::local::LocalVmStore::new(&state_dir);
    // Hold the per-VM lock before launching `outbe-up vm create`.
    let held_guard = store.lock_vm("dev").unwrap();

    let mut proc = Command::new(bin)
        .args([
            "vm",
            "create",
            "--name",
            "dev",
            "--image",
            image_dir.to_str().unwrap(),
            "--disk",
            raw_disk.to_str().unwrap(),
            "--bridge",
            "obr0",
            "--disk-gib",
            "1",
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
        ])
        .spawn()
        .expect("spawn outbe-up");

    // Give outbe-up time to enter the lock wait loop.
    thread::sleep(Duration::from_millis(150));

    let started = Instant::now();
    let pid = rustix::process::Pid::from_raw(i32::try_from(proc.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();

    let wait_deadline = Instant::now() + Duration::from_secs(3);
    let exit_status = loop {
        if let Some(status) = proc.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < wait_deadline,
            "outbe-up stayed blocked on lock after SIGTERM"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(!exit_status.success());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "SIGTERM took too long while waiting on lock"
    );
    drop(held_guard);

    // Zero VMM RPCs and no state file created.
    assert_eq!(
        server.rpc_count.load(Ordering::SeqCst),
        0,
        "canceled create must not send any VMM RPCs"
    );
    assert!(!state_dir.join("vms/dev.json").exists());
}

#[test]
fn f8_qemu_img_does_not_inherit_lock_fd_and_sigterm_kills_child_process_group() {
    let bin = env!("CARGO_BIN_EXE_outbe-up");
    let dir = tempfile::tempdir().unwrap();
    let vmm_home = dir.path().join("vmm");
    let sock = vmm_home.join("run/vmm.sock");
    let state_dir = dir.path().join("state");

    let image_dir = dir.path().join("img");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "aabbccddeeff0011".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();

    let raw_disk = dir.path().join("dev.img");
    fs::write(&raw_disk, b"raw-disk").unwrap();

    // Fake qemu-img records its own PID and /proc/self/fd targets, then waits for SIGTERM.
    let fake_bin = dir.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fd_log = dir.path().join("child-fds.txt");
    let pid_file = dir.path().join("child-pid.txt");
    let ready_flag = dir.path().join("child-ready");
    let fake_qemu = fake_bin.join("qemu-img");
    fs::write(
        &fake_qemu,
        format!(
            "#!/bin/sh\n\
             echo $$ > {}\n\
             ls -l /proc/self/fd > {} 2>&1 || true\n\
             touch {}\n\
             exec sleep 30\n",
            pid_file.display(),
            fd_log.display(),
            ready_flag.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_qemu, fs::Permissions::from_mode(0o755)).unwrap();

    let _server = FakeServer::start(sock.clone());
    let path_env = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut proc = Command::new(bin)
        .args([
            "vm",
            "create",
            "--name",
            "dev",
            "--image",
            image_dir.to_str().unwrap(),
            "--disk",
            raw_disk.to_str().unwrap(),
            "--bridge",
            "obr0",
            "--disk-gib",
            "1",
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
        ])
        .env("PATH", &path_env)
        .spawn()
        .expect("spawn outbe-up");

    // Wait until fake qemu-img has started and logged its fds.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready_flag.exists() {
        assert!(
            Instant::now() < deadline,
            "fake qemu-img did not start in time"
        );
        thread::sleep(Duration::from_millis(20));
    }

    // 1. O_CLOEXEC check: `.dev.lock` MUST NOT appear in child's `/proc/self/fd`.
    let fd_listing = fs::read_to_string(&fd_log).unwrap();
    assert!(
        !fd_listing.contains(".dev.lock"),
        "qemu-img inherited .dev.lock fd:\n{fd_listing}"
    );
    let child_pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // 2. Send SIGTERM to outbe-up; it must terminate fake qemu-img's process group and exit promptly.
    let pid = rustix::process::Pid::from_raw(i32::try_from(proc.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();

    let wait_deadline = Instant::now() + Duration::from_secs(5);
    let exit_status = loop {
        if let Some(status) = proc.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < wait_deadline,
            "outbe-up did not exit promptly after SIGTERM"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert!(!exit_status.success());

    // 3. Verify fake qemu-img process was killed and VM lock is immediately acquirable.
    let child_dead_deadline = Instant::now() + Duration::from_secs(2);
    while PathBuf::from(format!("/proc/{child_pid}")).exists() {
        if Instant::now() >= child_dead_deadline {
            if let Some(pid) = rustix::process::Pid::from_raw(child_pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            panic!("fake qemu-img (pid {child_pid}) survived SIGTERM to outbe-up");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let store = outbe_operator_vm::local::LocalVmStore::new(&state_dir);
    let _guard = store
        .lock_vm("dev")
        .expect("VM lock must be released immediately after outbe-up exits");
}
