use super::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn exporter_readiness_uses_exporter_status_route() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0_u8];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        assert!(request.starts_with(b"GET /status HTTP/1.1\r\n"));
        let body = r#"{"phase":"idle","current_bundle":null,"current_job":null,"last_error":null,"pending_jobs":0,"last_activity_ms_ago":0,"last_successful_reconcile_ms_ago":0}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let status = fetch_snapshot_exporter_status(address).unwrap();
    server.join().unwrap();
    assert_eq!(
        status.phase,
        outbe_ocomp::worker_observability::SnapshotExporterPhaseV1::Idle
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn status_probe_rejects_wrong_route_and_invalid_payload() {
    for response in [
        "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n{}",
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nnot-json",
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream.write_all(response.as_bytes()).unwrap();
        });
        assert!(fetch_snapshot_exporter_status(address).is_err());
        server.join().unwrap();
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn supervisor_readiness_requires_the_expected_registered_connected_workers() {
    let ready = SupervisorWorkerStatusV1 {
        registry_generation: 1,
        registered_workers: 1,
        connected_workers: 1,
        busy_workers: 0,
        accepted_leases: 0,
        queued_units: 0,
        max_workers: 4,
    };
    ensure_supervisor_status_ready(0, &ready, 1).unwrap();

    let mut missing = ready.clone();
    missing.registered_workers = 0;
    assert!(ensure_supervisor_status_ready(0, &missing, 1).is_err());

    let mut disconnected = ready;
    disconnected.connected_workers = 0;
    assert!(ensure_supervisor_status_ready(0, &disconnected, 1).is_err());
}
