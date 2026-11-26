use std::{
    fs,
    io::{self, BufReader, Cursor, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use super::{
    http::{parse_http_response, HTTP_READ_BUF_CAPACITY, MAX_HTTP_HEADER_BYTES},
    *,
};

#[test]
fn create_vm_request_matches_dstack_prpc_shape() {
    let app_id = calc_app_id(APP_COMPOSE_JSON);
    assert_eq!(app_id.len(), 40);
    assert!(app_id.bytes().all(|b| b.is_ascii_hexdigit()));

    let req = CreateVmRequest {
        name: "vm-devnet-validator-0".into(),
        image: "outbe-dev-0.1".into(),
        compose_file: APP_COMPOSE_JSON.into(),
        vcpu: 3,
        memory: 5 * 1024,
        disk_size: 40,
        ports: Vec::new(),
        app_id: Some(app_id.clone()),
        user_config: String::new(),
        hugepages: false,
        pin_numa: false,
        stopped: true,
        no_tee: false,
        networking: Some(NetworkingConfig {
            mode: "bridge".into(),
            bridge_name: "obr0".into(),
        }),
        annotations: BTreeMap::new(),
    };
    let value: Value = serde_json::to_value(&req).unwrap();
    assert_eq!(value["name"], json!("vm-devnet-validator-0"));
    assert_eq!(value["image"], json!("outbe-dev-0.1"));
    assert_eq!(value["vcpu"], json!(3));
    assert_eq!(value["memory"], json!(5120));
    assert_eq!(value["disk_size"], json!(40));
    assert_eq!(value["ports"], json!([]));
    assert_eq!(value["app_id"], json!(app_id));
    assert_eq!(value["user_config"], json!(""));
    assert_eq!(value["hugepages"], json!(false));
    assert_eq!(value["pin_numa"], json!(false));
    assert_eq!(value["stopped"], json!(true));
    assert_eq!(value["no_tee"], json!(false));
    assert_eq!(
        value["networking"],
        json!({"mode": "bridge", "bridge_name": "obr0"})
    );
    assert_eq!(value["annotations"], json!({}));
    // Ensure KMS/gateway/encrypted_env fields are absent for key_provider: none.
    assert!(value.get("kms_urls").is_none());
    assert!(value.get("gateway_urls").is_none());
    assert!(value.get("encrypted_env").is_none());
}

#[test]
fn id_and_status_requests_match_dstack_prpc_shape() {
    let id_req = serde_json::to_value(&IdMessage {
        id: "abc-123".into(),
    })
    .unwrap();
    assert_eq!(id_req, json!({"id": "abc-123"}));

    let update_req = serde_json::to_value(&UpdateVmRequest {
        id: "550e8400-e29b-41d4-a716-446655440000".into(),
        image: Some("outbe-vm-dev-0123456789abcdef".into()),
    })
    .unwrap();
    assert_eq!(
        update_req,
        json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "image": "outbe-vm-dev-0123456789abcdef",
        })
    );

    let status_req = serde_json::to_value(&StatusRequest {
        ids: vec!["vm-1".into()],
        brief: true,
        ..StatusRequest::default()
    })
    .unwrap();
    assert_eq!(
        status_req,
        json!({
            "ids": ["vm-1"],
            "brief": true,
            "keyword": "",
            "page": 0,
            "page_size": 0,
            "status": ""
        })
    );
}

#[test]
fn extract_primary_mac_prefers_bridge_and_rejects_invalid() {
    let response: GetInfoResponse = serde_json::from_value(json!({
        "found": true,
        "info": {
            "id": "vm-42",
            "name": "validator-0",
            "status": "stopped",
            "running": false,
            "interfaces": [
                { "mode": "user", "backend": "slirp", "mac": "52:54:00:11:22:33" },
                { "mode": "bridge", "backend": "tap_bridge", "mac": "52:54:00:AA:BB:CC", "bridge_name": "obr0" }
            ]
        }
    }))
    .unwrap();
    let info = response.info.unwrap();
    assert_eq!(extract_primary_mac(&info).unwrap(), "52:54:00:aa:bb:cc");

    let empty = VmInfo::default();
    assert!(extract_primary_mac(&empty).is_err());

    assert!(normalize_mac("not-a-mac").is_err());
    assert!(normalize_mac("52:54:00:11:22").is_err());
}

#[test]
fn remote_vmm_path_expands_tilde_safely() {
    assert_eq!(
        remote_vmm_path("~/.dstack-vmm", "run/vmm.sock"),
        "\"$HOME\"/'.dstack-vmm/run/vmm.sock'"
    );
    assert_eq!(
        remote_vmm_path("/srv/dstack-vmm/", "vm/abc/hda.img"),
        "'/srv/dstack-vmm/vm/abc/hda.img'"
    );
}

#[test]
fn parse_curl_http_response_handles_ok_empty_and_errors() {
    assert_eq!(
        parse_curl_http_response("CreateVm", "{\"id\":\"vm-1\"}\n200\n").unwrap(),
        "{\"id\":\"vm-1\"}"
    );
    assert_eq!(parse_curl_http_response("StartVm", "200\n").unwrap(), "");
    let err = parse_curl_http_response("CreateVm", "already exists\n400\n").unwrap_err();
    assert!(err.to_string().contains("HTTP 400"), "{err}");
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn validate_vm_id_rejects_shell_injection_and_non_uuid() {
    let valid = "550e8400-e29b-41d4-a716-446655440000";
    validate_vm_id(valid).unwrap();
    let cmd = build_serial_log_command("~/.dstack-vmm", valid, 40).unwrap();
    assert!(cmd.contains(&format!(
        "'http://localhost/logs?id={valid}&follow=false&ansi=false&lines=40'"
    )));

    let injection = "x'; printf REVIEW_INJECTION_OK >&2; fi; #";
    assert!(validate_vm_id(injection).is_err());
    assert!(build_serial_log_command("~/.dstack-vmm", injection, 40).is_err());
    assert!(validate_vm_id("vm-0001").is_err());
    assert!(validate_vm_id("").is_err());
}

#[test]
fn vm_info_is_stopped_matches_dstack_runtime_status_states() {
    let stopped = VmInfo {
        status: VmInfo::STATUS_STOPPED.into(),
        running: false,
        ..VmInfo::default()
    };
    assert!(stopped.is_stopped());
    assert!(stopped.is_administratively_stopped());
    stopped.ensure_never_started_and_stopped().unwrap();

    // Graceful ShutdownVm powers off guest while .started stays true -> status = "exited".
    let exited = VmInfo {
        status: VmInfo::STATUS_EXITED.into(),
        running: false,
        ..VmInfo::default()
    };
    assert!(exited.is_stopped());
    assert!(!exited.is_administratively_stopped());
    assert!(exited.ensure_never_started_and_stopped().is_err());

    for (status, running) in [
        (VmInfo::STATUS_RUNNING, true),
        (VmInfo::STATUS_STOPPING, true),
        (VmInfo::STATUS_REMOVING, false),
        (VmInfo::STATUS_STOPPED, true),
        ("unknown", false),
    ] {
        let info = VmInfo {
            status: status.into(),
            running,
            ..VmInfo::default()
        };
        assert!(
            !info.is_stopped(),
            "status={status} running={running} must not count as stopped"
        );
        assert!(info.ensure_never_started_and_stopped().is_err());
    }
}

#[test]
fn stub_transport_exercises_all_vmm_client_prpc_requests() {
    struct StubTransport {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl VmmTransport for StubTransport {
        fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
            let parsed: Value = serde_json::from_slice(payload)?;
            self.calls.lock().unwrap().push((method.to_owned(), parsed));
            let uuid = "550e8400-e29b-41d4-a716-446655440000";
            match method {
                "CreateVm" => Ok(format!("{{\"id\":\"{uuid}\"}}")),
                "GetInfo" => Ok(json!({
                    "found": true,
                    "info": {
                        "id": uuid,
                        "name": "dev-vm",
                        "status": "exited",
                        "uptime": "0s",
                        "running": false,
                        "configuration": { "image": "outbe-vm-dev-0123456789abcdef" },
                        "interfaces": [{ "mode": "bridge", "mac": "52:54:00:11:22:33" }]
                    }
                })
                .to_string()),
                "StartVm" | "ShutdownVm" | "StopVm" | "RemoveVm" => Ok("{}".into()),
                _ => Err(eyre::eyre!("unexpected method {method}")),
            }
        }

        fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
            Ok(format!("log:{vm_id}:{lines}\n"))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let client = VmmClient::new(
        StubTransport {
            calls: Arc::clone(&calls),
        },
        "stub",
    );

    let id = client
        .create_vm(&CreateVmRequest {
            name: "dev-vm".into(),
            image: "outbe-vm-dev-0123456789abcdef".into(),
            compose_file: APP_COMPOSE_JSON.into(),
            vcpu: 3,
            memory: 5120,
            disk_size: 10,
            ports: Vec::new(),
            app_id: Some(calc_app_id(APP_COMPOSE_JSON)),
            user_config: String::new(),
            hugepages: false,
            pin_numa: false,
            stopped: true,
            no_tee: false,
            networking: Some(NetworkingConfig {
                mode: "bridge".into(),
                bridge_name: "obr0".into(),
            }),
            annotations: BTreeMap::new(),
        })
        .unwrap();
    client.start_vm(&id).unwrap();
    client.shutdown_vm(&id).unwrap();
    client.stop_vm(&id).unwrap();
    let info = client.get_info(&id).unwrap();
    assert!(info.found);
    assert!(info.info.as_ref().unwrap().is_stopped());
    assert_eq!(
        info.info.as_ref().unwrap().image_name(),
        Some("outbe-vm-dev-0123456789abcdef")
    );
    assert_eq!(
        client.serial_log_tail(&id, 25).unwrap(),
        format!("log:{id}:25\n")
    );
    client.remove_vm(&id).unwrap();

    let recorded = calls.lock().unwrap();
    let methods: Vec<&str> = recorded.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "CreateVm",
            "StartVm",
            "ShutdownVm",
            "StopVm",
            "GetInfo",
            "RemoveVm"
        ]
    );
    assert_eq!(recorded[0].1["networking"]["mode"], json!("bridge"));
    assert_eq!(recorded[0].1["networking"]["bridge_name"], json!("obr0"));
    assert_eq!(recorded[2].1, json!({"id": id}));
}

#[test]
fn http_parser_handles_content_length_chunked_eof_and_errors() {
    // Content-Length response.
    let raw_cl =
        b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"ok\":\"true\"}";
    let parsed = parse_http_response(&mut Cursor::new(raw_cl)).unwrap();
    assert_eq!(parsed.status_code, 200);
    assert_eq!(parsed.body, br#"{"ok":"true"}"#);

    // Chunked response with chunk extension and trailer header.
    let raw_chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7;ext=1\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\nX-Trailer: yes\r\n\r\n";
    let parsed = parse_http_response(&mut Cursor::new(raw_chunked)).unwrap();
    assert_eq!(parsed.status_code, 200);
    assert_eq!(parsed.body, br#"{"a":1,"b":2}"#);

    // Connection-close EOF-framed response without Content-Length.
    let raw_eof = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"eof\":true}";
    let parsed = parse_http_response(&mut Cursor::new(raw_eof)).unwrap();
    assert_eq!(parsed.status_code, 200);
    assert_eq!(parsed.body, br#"{"eof":true}"#);

    // Truncated Content-Length fails closed.
    let raw_trunc = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort";
    assert!(parse_http_response(&mut Cursor::new(raw_trunc)).is_err());

    // Conflicting Content-Length fails closed.
    let raw_conflict = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\n1234";
    assert!(parse_http_response(&mut Cursor::new(raw_conflict)).is_err());
}

#[test]
fn http_header_limit_rejects_newline_less_stream_before_allocating_past_64kib() {
    /// Endless newline-less byte source counting how many bytes were read.
    struct EndlessNoNewline {
        read_bytes: Arc<Mutex<usize>>,
    }
    impl Read for EndlessNoNewline {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(b'A');
            *self.read_bytes.lock().unwrap() += buf.len();
            Ok(buf.len())
        }
    }

    let read_bytes = Arc::new(Mutex::new(0usize));
    let mut reader = BufReader::with_capacity(
        HTTP_READ_BUF_CAPACITY,
        EndlessNoNewline {
            read_bytes: Arc::clone(&read_bytes),
        },
    );
    let err = parse_http_response(&mut reader).unwrap_err();
    assert!(
        err.to_string().contains("exceed maximum"),
        "expected header size cap error, got: {err}"
    );
    let total_read = *read_bytes.lock().unwrap();
    assert!(
        total_read <= MAX_HTTP_HEADER_BYTES + HTTP_READ_BUF_CAPACITY,
        "reader consumed {total_read} bytes past header cap {MAX_HTTP_HEADER_BYTES}"
    );
}

/// Return the `Content-Length` value of a request header block (the last one wins).
fn header_content_length(headers: &str) -> usize {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse().unwrap_or(0))
        .next_back()
        .unwrap_or(0)
}

/// Return the total request length (headers plus body) once the header block is complete.
fn request_total_len(buf: &[u8]) -> Option<usize> {
    let pos = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let headers = String::from_utf8_lossy(&buf[..pos]);
    Some(pos + 4 + header_content_length(&headers))
}

/// Read one HTTP request (headers plus `Content-Length` body bytes) and return its raw bytes.
fn read_http_request(stream: &mut UnixStream) -> Vec<u8> {
    let mut req_buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut total_len = None;
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        req_buf.extend_from_slice(&chunk[..n]);
        if total_len.is_none() {
            total_len = request_total_len(&req_buf);
        }
        if total_len.is_some_and(|len| req_buf.len() >= len) {
            break;
        }
    }
    req_buf
}

fn spawn_one_shot_unix_server(
    sock_path: &Path,
    raw_response: Vec<u8>,
    delay: Duration,
) -> thread::JoinHandle<String> {
    let listener = UnixListener::bind(sock_path).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let req_buf = read_http_request(&mut stream);
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        let _ = stream.write_all(&raw_response);
        let _ = stream.flush();
        String::from_utf8_lossy(&req_buf).into_owned()
    })
}

/// Set the accept backlog of a listening socket. Linux admits `backlog + 1` pending peers, so a
/// tiny backlog makes `connect()` block after a few peers, independent of `somaxconn`.
fn set_listen_backlog(listener: &UnixListener, backlog: i32) {
    rustix::net::listen(listener, backlog)
        .unwrap_or_else(|err| panic!("listen(fd, {backlog}) failed: {err}"));
}

#[test]
fn local_unix_transport_enforces_deadline_on_blocked_connect() {
    const MAX_FILLERS: usize = 16;
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("full-backlog.sock");

    // Bind a Unix listener that NEVER calls accept() and shrink its backlog to zero. Fill the
    // backlog on a helper thread until one `connect()` blocks in the kernel.
    let listener = UnixListener::bind(&sock).unwrap();
    set_listen_backlog(&listener, 0);
    let (tx, rx) = mpsc::channel();
    let filler_sock = sock.clone();
    let filler_handle = thread::spawn(move || {
        for _ in 0..MAX_FILLERS {
            let Ok(stream) = UnixStream::connect(&filler_sock) else {
                break;
            };
            if tx.send(stream).is_err() {
                break;
            }
        }
    });

    let mut fillers = Vec::new();
    let mut saturated = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(stream) => fillers.push(stream),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                saturated = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        saturated && !fillers.is_empty(),
        "expected listener backlog to saturate (filled {} connections, max {MAX_FILLERS})",
        fillers.len()
    );

    let started = Instant::now();
    let transport =
        LocalUnixVmmTransport::new(&sock, dir.path()).with_timeout(Duration::from_millis(200));
    let client = VmmClient::new(transport, "blocked-connect");
    let err = client.get_meta().unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "blocked connect exceeded 200ms deadline: took {elapsed:?} ({err:#})"
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("timed out connecting"),
        "expected connect timeout error, got: {msg}"
    );

    // Close listener and drop backlog connections so the blocked helper threads exit cleanly.
    drop(rx);
    drop(listener);
    drop(fillers);
    let _ = filler_handle.join();
}

#[test]
fn local_unix_transport_enforces_total_deadline_against_trickle_response() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("trickle.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    // Trickle 1 byte every 50ms for 4 seconds (80 bytes): inactivity never exceeds 50ms,
    // yet a 250ms total deadline must abort promptly well under 1s.
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut req = [0u8; 1024];
        let _ = stream.read(&mut req);
        let header = b"HTTP/1.1 200 OK\r\nContent-Length: 80\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(header);
        let _ = stream.flush();
        for _ in 0..80 {
            thread::sleep(Duration::from_millis(50));
            if stream.write_all(b"x").is_err() {
                break;
            }
            let _ = stream.flush();
        }
    });

    let started = Instant::now();
    let transport =
        LocalUnixVmmTransport::new(&sock, dir.path()).with_timeout(Duration::from_millis(250));
    let client = VmmClient::new(transport, "trickle-test");
    let err = client.get_meta().unwrap_err();
    let elapsed = started.elapsed();
    let _ = handle.join();

    assert!(
        elapsed < Duration::from_secs(1),
        "trickle bypassed total deadline: took {elapsed:?} (err: {err:#})"
    );
}

#[test]
fn local_unix_transport_handles_chunked_non_2xx_and_read_timeout() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Chunked 200 response over real Unix socket, verifying Connection: close header.
    let sock_chunked = dir.path().join("chunked.sock");
    let chunked_resp = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n15\r\n{\"found\":false,\"info\"\r\n6\r\n:null}\r\n0\r\n\r\n".to_vec();
    let handle = spawn_one_shot_unix_server(&sock_chunked, chunked_resp, Duration::ZERO);
    let transport = LocalUnixVmmTransport::new(&sock_chunked, dir.path());
    let client = VmmClient::new(transport, "local-test");
    let resp = client
        .get_info("550e8400-e29b-41d4-a716-446655440000")
        .unwrap();
    assert!(!resp.found);
    assert!(resp.is_gone());
    let req_text = handle.join().unwrap();
    assert!(
        req_text
            .to_ascii_lowercase()
            .contains("connection: close\r\n"),
        "request must send Connection: close:\n{req_text}"
    );

    // 2. Non-2xx response includes HTTP status and body in the error.
    let sock_err = dir.path().join("err.sock");
    let err_resp = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 27\r\n\r\n{\"error\":\"disk full on host\"}".to_vec();
    let handle = spawn_one_shot_unix_server(&sock_err, err_resp, Duration::ZERO);
    let client = VmmClient::new(
        LocalUnixVmmTransport::new(&sock_err, dir.path()),
        "local-err",
    );
    let err = client
        .start_vm("550e8400-e29b-41d4-a716-446655440000")
        .unwrap_err();
    let _ = handle.join();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "{msg}");
    assert!(msg.contains("disk full on host"), "{msg}");

    // 3. Read timeout triggers when server stalls past io_timeout.
    let sock_timeout = dir.path().join("timeout.sock");
    let handle = spawn_one_shot_unix_server(
        &sock_timeout,
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        Duration::from_millis(400),
    );
    let transport = LocalUnixVmmTransport::new(&sock_timeout, dir.path())
        .with_timeout(Duration::from_millis(80));
    let client = VmmClient::new(transport, "local-timeout");
    let err = client.get_meta().unwrap_err();
    let _ = handle.join();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("timed out") || msg.contains("deadline") || msg.contains("read HTTP response"),
        "expected timeout error, got: {msg}"
    );
}

#[test]
fn local_unix_transport_parses_real_amd_vmm_captures() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vmm");

    // Real GetMeta capture.
    let raw_meta = fs::read(fixtures.join("GetMeta.http")).unwrap();
    let sock = dir.path().join("meta.sock");
    let h = spawn_one_shot_unix_server(&sock, raw_meta, Duration::ZERO);
    let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-meta");
    let meta = client.get_meta().unwrap();
    let _ = h.join();
    assert_eq!(meta["networking"]["default_mode"], json!("user"));
    assert_eq!(meta["networking"]["default_bridge"], json!("obr0"));

    // Real Status capture.
    let raw_status = fs::read(fixtures.join("Status.http")).unwrap();
    let sock = dir.path().join("status.sock");
    let h = spawn_one_shot_unix_server(&sock, raw_status, Duration::ZERO);
    let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-status");
    let status = client.status(&StatusRequest::default()).unwrap();
    let _ = h.join();
    assert_eq!(status.vms.len(), 4);
    assert_eq!(status.total, 4);
    assert_eq!(status.vms[0].status, "running");
    assert!(status.vms[0].running);
    assert!(!status.vms[0].uptime.is_empty());
    assert_eq!(
        status.vms[0].image_name(),
        Some("outbe-vm-dev-3485ca2592b7a0c3")
    );

    // Real GetInfo capture for existing VM.
    let raw_info = fs::read(fixtures.join("GetInfo.http")).unwrap();
    let sock = dir.path().join("info.sock");
    let h = spawn_one_shot_unix_server(&sock, raw_info, Duration::ZERO);
    let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-info");
    let resp = client
        .get_info("cdbc5a01-481f-47f1-8474-cdac63ed9f32")
        .unwrap();
    let _ = h.join();
    assert!(resp.found);
    assert!(!resp.is_gone());
    let info = resp.info.unwrap();
    assert_eq!(info.name, "final-amd-f36afc309-validator-0");
    assert_eq!(extract_primary_mac(&info).unwrap(), "be:f7:7b:2c:d1:85");
    assert_eq!(info.image_name(), Some("outbe-vm-dev-3485ca2592b7a0c3"));
    assert_eq!(info.configuration.as_ref().unwrap().vcpu, 3);
    assert_eq!(info.configuration.as_ref().unwrap().memory, 5120);
    assert_eq!(
        info.effective_app_id(),
        Some("7c85e0bd1f1a9b203bdc20bceb46f77fd35e830b")
    );

    // Real GetInfo-unknown capture (HTTP 200 with found:false, info:null).
    let raw_unknown = fs::read(fixtures.join("GetInfo-unknown.http")).unwrap();
    let sock = dir.path().join("unknown.sock");
    let h = spawn_one_shot_unix_server(&sock, raw_unknown, Duration::ZERO);
    let client = VmmClient::new(LocalUnixVmmTransport::new(&sock, dir.path()), "amd-unknown");
    let resp = client
        .get_info("82551677-4885-4acd-9f1d-758512c7e13b")
        .unwrap();
    let _ = h.join();
    assert!(!resp.found);
    assert!(resp.info.is_none());
    assert!(resp.is_gone());

    // Real UnknownMethod capture (HTTP 404 with JSON error body).
    let raw_404 = fs::read(fixtures.join("UnknownMethod.http")).unwrap();
    let sock = dir.path().join("404.sock");
    let h = spawn_one_shot_unix_server(&sock, raw_404, Duration::ZERO);
    let transport = LocalUnixVmmTransport::new(&sock, dir.path());
    let err = transport
        .prpc_post("S1ReadOnlyUnknownMethod", b"{}")
        .unwrap_err();
    let _ = h.join();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 404"), "{msg}");
    assert!(msg.contains("Service not found"), "{msg}");
}
