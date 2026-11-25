//! End-to-end executing test of `outbe-operator-vm::local` against a real Unix domain socket
//! fake VMM server speaking dstack pRPC JSON shapes.

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use outbe_operator_vm::{
    disk::stage_local_disk,
    local::{
        create_local_vm_with_disk_stager, logs_local_vm, remove_local_vm, start_local_vm,
        status_all_local_vms, status_local_vm, stop_local_vm, CreateLocalVmOptions, LocalVmPhase,
        LocalVmStore, RemoveLocalVmOptions, CREATE_TOKEN_ANNOTATION,
    },
    vmm::{LocalUnixVmmTransport, VmInfo, VmmClient},
};
use serde_json::{json, Value};

const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440123";
const TEST_MAC: &str = "52:54:00:12:34:56";

#[derive(Clone)]
struct ServerVm {
    id: String,
    name: String,
    image: String,
    vcpu: u32,
    memory: u32,
    app_id: String,
    status: String,
    running: bool,
    stopped_flag: bool,
    uptime: String,
    mac: String,
    annotations: BTreeMap<String, String>,
    remove_polls_left: usize,
}

#[derive(Default)]
struct ServerState {
    vms: BTreeMap<String, ServerVm>,
    methods_seen: Vec<String>,
    create_payload: Option<Value>,
    drop_create_response_once: bool,
    trickle_get_info_once: bool,
}

struct FakeUnixVmmServer {
    socket_path: PathBuf,
    state: Arc<Mutex<ServerState>>,
    stop_flag: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeUnixVmmServer {
    fn start(socket_path: PathBuf) -> Self {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let state = Arc::new(Mutex::new(ServerState::default()));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let state_bg = Arc::clone(&state);
        let stop_bg = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        handle_connection(&mut stream, &state_bg);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            socket_path,
            state,
            stop_flag,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeUnixVmmServer {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn handle_connection(stream: &mut std::os::unix::net::UnixStream, state: &Arc<Mutex<ServerState>>) {
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
    let Some(end) = header_end else {
        return;
    };
    let head = String::from_utf8_lossy(&raw[..end]);
    let request_line = head.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let http_method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let body_bytes = &raw[end..end + content_len];

    if http_method == "GET" && target.starts_with("/logs?") {
        let log_body = "serial line 1\nserial line 2\n";
        // Return chunked encoding on /logs to exercise the chunked parser end-to-end too.
        let chunk_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{log_body}\r\n0\r\n\r\n",
            log_body.len()
        );
        let _ = stream.write_all(chunk_resp.as_bytes());
        let _ = stream.flush();
        return;
    }

    let prpc_method = target
        .strip_prefix("/prpc/")
        .and_then(|rest| rest.split('?').next())
        .unwrap_or("");
    let req: Value = serde_json::from_slice(body_bytes).unwrap_or(json!({}));

    let mut guard = state.lock().unwrap();
    guard.methods_seen.push(prpc_method.to_owned());

    if prpc_method == "GetInfo" && guard.trickle_get_info_once {
        guard.trickle_get_info_once = false;
        drop(guard);
        let header = b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\n";
        let _ = stream.write_all(header);
        let _ = stream.flush();
        for _ in 0..100 {
            thread::sleep(Duration::from_millis(80));
            if stream.write_all(b" ").is_err() {
                break;
            }
            let _ = stream.flush();
        }
        return;
    }

    let (status_line, resp_body) = match prpc_method {
        "Status" => {
            let brief = req["brief"].as_bool().unwrap_or(false);
            let keyword = req["keyword"].as_str().unwrap_or("");
            let page = req["page"].as_u64().unwrap_or(0) as usize;
            let page_size = req["page_size"].as_u64().unwrap_or(0) as usize;

            let filtered: Vec<&ServerVm> = guard
                .vms
                .values()
                .filter(|vm| keyword.is_empty() || vm.name.contains(keyword))
                .collect();
            let total = filtered.len() as u32;
            let page_slice: Vec<&ServerVm> = if page == 0 || page_size == 0 {
                filtered
            } else {
                filtered
                    .into_iter()
                    .skip((page - 1) * page_size)
                    .take(page_size)
                    .collect()
            };
            let vms: Vec<Value> = page_slice
                .into_iter()
                .map(|vm| {
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
                        "id": vm.id,
                        "name": vm.name,
                        "status": vm.status,
                        "uptime": vm.uptime,
                        "app_id": vm.app_id,
                        "running": vm.running,
                        "configuration": config,
                        "interfaces": [{ "mode": "bridge", "backend": "tap_bridge", "mac": vm.mac, "bridge_name": "obr0" }]
                    })
                })
                .collect();
            (
                "HTTP/1.1 200 OK",
                json!({ "vms": vms, "total": total }).to_string(),
            )
        }
        "CreateVm" => {
            guard.create_payload = Some(req.clone());
            let name = req["name"].as_str().unwrap().to_owned();
            let image = req["image"].as_str().unwrap().to_owned();
            let vcpu = req["vcpu"].as_u64().unwrap() as u32;
            let memory = req["memory"].as_u64().unwrap() as u32;
            let app_id = req["app_id"].as_str().unwrap_or("").to_owned();
            let annotations: BTreeMap<String, String> = req["annotations"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            guard.vms.insert(
                TEST_UUID.into(),
                ServerVm {
                    id: TEST_UUID.into(),
                    name,
                    image,
                    vcpu,
                    memory,
                    app_id,
                    status: VmInfo::STATUS_STOPPED.into(),
                    running: false,
                    stopped_flag: true,
                    uptime: String::new(),
                    mac: TEST_MAC.into(),
                    annotations,
                    remove_polls_left: 2,
                },
            );
            if guard.drop_create_response_once {
                guard.drop_create_response_once = false;
                // Close without responding to simulate crash/EOF after server-side creation.
                return;
            }
            ("HTTP/1.1 200 OK", json!({ "id": TEST_UUID }).to_string())
        }
        "GetInfo" => {
            let id = req["id"].as_str().unwrap_or("");
            let mut drop_vm = false;
            let body = match guard.vms.get_mut(id) {
                Some(vm) => {
                    if vm.status == VmInfo::STATUS_REMOVING {
                        if vm.remove_polls_left == 0 {
                            drop_vm = true;
                            json!({ "found": false, "info": null })
                        } else {
                            vm.remove_polls_left -= 1;
                            json!({
                                "found": true,
                                "info": {
                                    "id": vm.id,
                                    "name": vm.name,
                                    "status": vm.status,
                                    "uptime": "",
                                    "app_id": vm.app_id,
                                    "running": false,
                                    "configuration": {
                                        "image": vm.image,
                                        "vcpu": vm.vcpu,
                                        "memory": vm.memory,
                                        "app_id": vm.app_id,
                                        "stopped": vm.stopped_flag,
                                        "annotations": vm.annotations,
                                    },
                                    "interfaces": [{ "mode": "bridge", "mac": vm.mac }]
                                }
                            })
                        }
                    } else {
                        json!({
                            "found": true,
                            "info": {
                                "id": vm.id,
                                "name": vm.name,
                                "status": vm.status,
                                "uptime": vm.uptime,
                                "app_id": vm.app_id,
                                "running": vm.running,
                                "configuration": {
                                    "image": vm.image,
                                    "vcpu": vm.vcpu,
                                    "memory": vm.memory,
                                    "app_id": vm.app_id,
                                    "stopped": vm.stopped_flag,
                                    "annotations": vm.annotations,
                                },
                                "interfaces": [{ "mode": "bridge", "mac": vm.mac }]
                            }
                        })
                    }
                }
                None => json!({ "found": false, "info": null }),
            };
            if drop_vm {
                guard.vms.remove(id);
            }
            ("HTTP/1.1 200 OK", body.to_string())
        }
        "StartVm" => {
            let id = req["id"].as_str().unwrap_or("");
            if let Some(vm) = guard.vms.get_mut(id) {
                vm.running = true;
                vm.stopped_flag = false;
                vm.status = VmInfo::STATUS_RUNNING.into();
                vm.uptime = "0h 1m 12s".into();
                ("HTTP/1.1 200 OK", "{}".to_owned())
            } else {
                (
                    "HTTP/1.1 404 Not Found",
                    json!({"error": "VM not found"}).to_string(),
                )
            }
        }
        "ShutdownVm" => {
            let id = req["id"].as_str().unwrap_or("");
            if let Some(vm) = guard.vms.get_mut(id) {
                vm.running = false;
                vm.status = VmInfo::STATUS_EXITED.into();
                vm.uptime = String::new();
                ("HTTP/1.1 200 OK", "{}".to_owned())
            } else {
                (
                    "HTTP/1.1 404 Not Found",
                    json!({"error": "VM not found"}).to_string(),
                )
            }
        }
        "StopVm" => {
            let id = req["id"].as_str().unwrap_or("");
            if let Some(vm) = guard.vms.get_mut(id) {
                vm.running = false;
                vm.stopped_flag = true;
                vm.status = VmInfo::STATUS_STOPPED.into();
                vm.uptime = String::new();
                ("HTTP/1.1 200 OK", "{}".to_owned())
            } else {
                (
                    "HTTP/1.1 404 Not Found",
                    json!({"error": "VM not found"}).to_string(),
                )
            }
        }
        "RemoveVm" => {
            let id = req["id"].as_str().unwrap_or("");
            if let Some(vm) = guard.vms.get_mut(id) {
                vm.running = false;
                vm.status = VmInfo::STATUS_REMOVING.into();
                ("HTTP/1.1 200 OK", "{}".to_owned())
            } else {
                (
                    "HTTP/1.1 404 Not Found",
                    json!({"error": "VM not found"}).to_string(),
                )
            }
        }
        _ => (
            "HTTP/1.1 404 Not Found",
            json!({ "error": format!("Service not found: {prpc_method}") }).to_string(),
        ),
    };

    let response = format!(
        "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
        resp_body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[test]
fn full_local_vm_lifecycle_over_fake_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let vmm_home = dir.path().join("dstack-vmm");
    let sock_path = vmm_home.join("run/vmm.sock");
    let state_dir = dir.path().join("state");
    let leases_file = dir.path().join("dhcp-leases");

    let image_dir = dir.path().join("guest-image");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "fedcba9876543210".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{\"os\":\"outbe\"}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();

    let raw_disk = dir.path().join("dev.img");
    let f = fs::File::create(&raw_disk).unwrap();
    f.set_len(16 * 1024 * 1024).unwrap();
    drop(f);

    let has_qemu_img = Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());

    let server = FakeUnixVmmServer::start(sock_path.clone());
    // F1: drop the first CreateVm response after server-side creation to prove recovery over UDS.
    server.state.lock().unwrap().drop_create_response_once = true;

    let store = LocalVmStore::new(&state_dir);
    let vmm = VmmClient::new(
        LocalUnixVmmTransport::new(&sock_path, &vmm_home),
        "fake-unix-vmm",
    );

    let opts = CreateLocalVmOptions {
        name: "dev".into(),
        image_dir,
        image_name: None,
        flavor: "dev".into(),
        disk_source: raw_disk,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 1,
        bridge: "obr0".into(),
        app_compose_path: None,
        intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
        no_tee: false,
        vmm_socket: sock_path.clone(),
        vmm_home: vmm_home.clone(),
        retry_create: false,
        timeout: None,
        recovery_grace: Duration::from_secs(2),
        poll_interval: Duration::from_millis(10),
    };

    // First create fails with EOF after server registered VM, leaving `Creating` journal on disk.
    assert!(create_local_vm_with_disk_stager(&store, &vmm, &opts, stage_local_disk).is_err());

    // Retry adopts the exact token-matched + config-matched VM and completes creation.
    let created = if has_qemu_img {
        create_local_vm_with_disk_stager(&store, &vmm, &opts, stage_local_disk).unwrap()
    } else {
        eprintln!("NOTE: qemu-img absent in PATH; exercising fake disk stager");
        create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            let p = home.join("vm").join(id).join("hda.img");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"qcow2-stub").unwrap();
            Ok(p)
        })
        .unwrap()
    };

    assert_eq!(created.vm_id, TEST_UUID);
    assert_eq!(created.image_name, "outbe-vm-dev-fedcba9876543210");
    assert_eq!(created.mac, TEST_MAC);
    assert_eq!(created.phase, LocalVmPhase::Ready);
    assert!(vmm_home.join(format!("vm/{TEST_UUID}/hda.img")).is_file());

    // Confirm CreateVm explicitly passed bridge networking, stopped=true, and create-token annotation.
    let create_req = server.state.lock().unwrap().create_payload.clone().unwrap();
    assert_eq!(
        create_req["networking"],
        json!({"mode": "bridge", "bridge_name": "obr0"})
    );
    assert_eq!(create_req["stopped"], json!(true));
    assert_eq!(create_req["memory"], json!(5120));
    assert!(create_req["annotations"][CREATE_TOKEN_ANNOTATION]
        .as_str()
        .is_some_and(|t| t.len() == 32));

    // Start VM.
    let started = start_local_vm(&store, &vmm, "dev").unwrap();
    assert!(!started.already_running);
    assert_eq!(started.vmm_status, VmInfo::STATUS_RUNNING);

    // F4: stop_local_vm timeout bounds the initial GetInfo even against a slow trickle response.
    server.state.lock().unwrap().trickle_get_info_once = true;
    let trickle_start = Instant::now();
    let trickle_err = stop_local_vm(
        &store,
        &vmm,
        "dev",
        false,
        Duration::from_millis(300),
        Duration::from_millis(10),
    )
    .unwrap_err();
    assert!(
        trickle_start.elapsed() < Duration::from_secs(2),
        "stop_local_vm exceeded operation timeout on trickled GetInfo: {:?} ({trickle_err:#})",
        trickle_start.elapsed()
    );

    // Status without leases file -> IP unknown (None), then with leases file -> resolved IP.
    let status_no_ip = status_local_vm(&store, &vmm, "dev", &leases_file).unwrap();
    assert_eq!(status_no_ip.vmm_status, "running");
    assert!(status_no_ip.running);
    assert_eq!(status_no_ip.uptime, "0h 1m 12s");
    assert_eq!(status_no_ip.ip, None);
    assert_eq!(status_no_ip.image_name, "outbe-vm-dev-fedcba9876543210");

    fs::write(
        &leases_file,
        format!("1795598734 {TEST_MAC} 10.77.0.88 dev *\n"),
    )
    .unwrap();
    let all = status_all_local_vms(&store, &vmm, &leases_file).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].ip, Some(Ipv4Addr::new(10, 77, 0, 88)));

    // Logs fallback over chunked HTTP /logs when serial.log is absent, then direct file read.
    let logs_http = logs_local_vm(&store, &vmm, "dev", 10).unwrap();
    assert!(logs_http.log.contains("serial line 2"));

    let serial_log_path = vmm_home.join(format!("vm/{TEST_UUID}/serial.log"));
    fs::write(&serial_log_path, "line1\nline2\nline3\n").unwrap();
    let logs_file = logs_local_vm(&store, &vmm, "dev", 2).unwrap();
    assert_eq!(logs_file.log, "line2\nline3\n");

    // Graceful stop calls ShutdownVm and detects `exited` as stopped.
    let stopped = stop_local_vm(
        &store,
        &vmm,
        "dev",
        false,
        Duration::from_secs(2),
        Duration::from_millis(10),
    )
    .unwrap();
    assert!(!stopped.forced);
    assert_eq!(stopped.vmm_status, VmInfo::STATUS_EXITED);
    assert!(stopped.warning.is_none());

    // Remove waits across async polls, removes state file, leaves published image intact.
    let removed = remove_local_vm(
        &store,
        &vmm,
        "dev",
        RemoveLocalVmOptions {
            force: false,
            forget_intent: false,
            timeout: Duration::from_secs(2),
            recovery_grace: Duration::from_millis(100),
            poll_interval: Duration::from_millis(10),
        },
    )
    .unwrap();
    assert_eq!(removed.vm_id, TEST_UUID);
    assert!(!Path::new(&removed.state_file).exists());
    assert!(vmm_home.join("image").join(&created.image_name).is_dir());

    let seen = server.state.lock().unwrap().methods_seen.clone();
    assert_eq!(
        seen.iter().filter(|m| m.as_str() == "CreateVm").count(),
        1,
        "lost-response recovery must not send a second CreateVm"
    );
    assert!(seen.contains(&"ShutdownVm".to_owned()));
    assert!(!seen.contains(&"StopVm".to_owned()));
    assert!(seen.contains(&"RemoveVm".to_owned()));
}
