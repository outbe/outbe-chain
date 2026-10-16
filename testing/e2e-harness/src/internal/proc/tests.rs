use std::os::unix::fs::PermissionsExt;

use super::enclave::{enclave_listener_ready, spawn_enclave_before};
use super::enclave_launch::{
    build_enclave_command, build_native_command, select_sgx_enclave_device,
};
use super::sgx_image::{
    parse_test_sgx_measurement, TEST_ENCLAVE_IMAGE, TEST_ENCLAVE_IMAGE_BUILD_ARGS,
};
use super::*;

fn startup_spec(root: &Path, port: u16, binary: &str) -> EnclaveSpec {
    EnclaveSpec {
        name: "startup-fixture".into(),
        tee_port: port,
        enclave_bin: binary.into(),
        signing_key: root.join("unused.pem"),
        network_descriptor: None,
        dev_network_binding: None,
        launch: EnclaveLaunch::NativeHost,
        sudo: false,
        pass_sgx_devices: false,
        remote_attestation: TestRemoteAttestation::None,
        dkg_seed: None,
        seal: None,
        log_path: root.join("enclave.log"),
        debug: false,
    }
}

#[test]
fn enclave_readiness_requires_exact_current_marker_and_live_child() {
    use std::os::unix::process::ExitStatusExt as _;
    let marker = "outbe-tee-enclave: listening on tcp://127.0.0.1:7000 (attestation: none; enclave identity: public)\n";
    assert!(enclave_listener_ready(None, marker, 7000, true).unwrap());
    assert!(!enclave_listener_ready(None, marker.trim_end(), 7000, true).unwrap());
    assert!(!enclave_listener_ready(None, marker, 7001, true).unwrap());
    assert!(!enclave_listener_ready(None, marker, 7000, false).unwrap());
    assert!(!enclave_listener_ready(None, "Gramine is starting", 7000, true).unwrap());
    assert!(
        !enclave_listener_ready(None, &format!("old diagnostic: {marker}"), 7000, true).unwrap()
    );
    for status in [
        ExitStatus::from_raw(0),
        ExitStatus::from_raw(23 << 8),
        ExitStatus::from_raw(9),
    ] {
        assert!(enclave_listener_ready(Some(status), marker, 7000, true).is_err());
    }
}

#[test]
fn enclave_readiness_rejects_foreign_listener_before_native_or_docker_spawn() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = tempfile::tempdir().unwrap();
    for docker in [false, true] {
        let mut spec = startup_spec(root.path(), port, "/nonexistent-enclave");
        if docker {
            spec.launch = EnclaveLaunch::Gramine {
                image_id: DockerImageId::from_inspect_output(&format!(
                    "sha256:{}",
                    "ab".repeat(32)
                ))
                .unwrap(),
            };
        }
        let error = spawn_enclave_ready(spec, std::time::Instant::now() + Duration::from_secs(2))
            .unwrap_err();
        assert!(error.to_string().contains("already occupied"), "{error:#}");
        assert!(!root.path().join("enclave.log").exists());
    }
    assert!(TcpStream::connect(listener.local_addr().unwrap()).is_ok());
}

#[test]
fn enclave_readiness_expired_deadline_never_spawns() {
    let root = tempfile::tempdir().unwrap();
    let error = spawn_enclave_ready(
        startup_spec(root.path(), 0, "/nonexistent-enclave"),
        std::time::Instant::now() - Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.to_string().contains("deadline already expired"));
    assert!(!root.path().join("enclave.log").exists());
}

#[test]
fn enclave_deadline_expiring_during_preparation_prevents_child_spawn() {
    let root = tempfile::tempdir().unwrap();
    let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let error = spawn_enclave_before(
        startup_spec(root.path(), port, "/nonexistent-enclave"),
        Some(std::time::Instant::now() - Duration::from_secs(1)),
    )
    .unwrap_err();
    // Command preparation completed, but the nonexistent executable was
    // never spawned: the failure is the deadline, not an exec error.
    assert!(format!("{error:#}").contains("deadline expired during launch preparation"));
    assert!(root.path().join("enclave.log").exists());
}

#[test]
fn enclave_readiness_preserves_early_exit_before_cleanup() {
    for binary in ["/usr/bin/true", "/usr/bin/false"] {
        let root = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = spawn_enclave_ready(
            startup_spec(root.path(), port, binary),
            std::time::Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("child exited before readiness"),
            "{error:#}"
        );
        let evidence = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(evidence).unwrap()).unwrap();
        assert_eq!(record["phase"], "failed");
        assert!(record["child_status_before_cleanup"]
            .as_str()
            .unwrap()
            .contains("Some"));
    }
}

#[test]
fn enclave_readiness_accepts_delayed_current_bind_and_cleans_owned_child() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("listener-fixture");
    fs::write(&binary, br#"#!/usr/bin/env python3
import socket, sys, time
time.sleep(0.1)
endpoint = sys.argv[2]
host, port = endpoint.rsplit(':', 1)
listener = socket.socket()
listener.bind((host, int(port)))
listener.listen()
print(f'outbe-tee-enclave: listening on tcp://{endpoint} (attestation: none; enclave identity: fixture)', file=sys.stderr, flush=True)
while True:
    connection, _ = listener.accept()
    connection.close()
"#).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let mut guard = spawn_enclave_ready(
        startup_spec(root.path(), address.port(), binary.to_str().unwrap()),
        std::time::Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert!(guard.child.exit_status().unwrap().is_none());
    assert!(TcpStream::connect(address).is_ok());
    let evidence = root
        .path()
        .join(format!("enclave-startup-{}.json", guard.child.pid()));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(evidence).unwrap()).unwrap();
    assert_eq!(report["phase"], "listening");
    drop(guard);
    assert!(TcpStream::connect(address).is_err());
}

#[test]
fn enclave_readiness_timeout_keeps_original_alive_observation() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("non-listening-fixture");
    fs::write(&binary, b"#!/bin/sh\nexec sleep 60\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let error = spawn_enclave_ready(
        startup_spec(root.path(), port, binary.to_str().unwrap()),
        std::time::Instant::now() + Duration::from_millis(500),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("deadline"), "{error:#}");
    let evidence = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&fs::read(evidence).unwrap()).unwrap();
    assert_eq!(record["phase"], "failed");
    assert_eq!(record["child_status_before_cleanup"], "Ok(None)");
}

#[test]
fn child_guard_interrupts_and_reaps_the_operator_ctrl_c_path() {
    let mut command = Command::new("sh");
    command.args(["-c", "trap 'exit 0' INT; while true; do sleep 0.05; done"]);
    let mut child = ChildGuard::spawn("interrupt fixture", command).expect("spawn fixture");
    sleep(Duration::from_millis(50));

    child.interrupt();

    assert!(child.exited());
}

#[test]
fn restart_observation_reaps_the_same_owned_enclave_launcher() {
    let mut command = Command::new("sleep");
    command.arg("60");
    let child = ChildGuard::spawn("owned restart observation", command).unwrap();
    let pid = child.pid();
    let mut enclave = EnclaveGuard {
        child,
        docker: None,
    };
    assert_eq!(enclave.pid(), pid);
    assert!(enclave.exit_status().unwrap().is_none());
    let status = enclave.stop_and_reap().unwrap();
    assert_eq!(enclave.exit_status().unwrap(), Some(status));
    assert_eq!(enclave.stop_and_reap().unwrap(), status);
    assert_eq!(enclave.pid(), pid);
}

#[test]
fn abrupt_owned_fault_is_reaped_and_cannot_be_repeated_as_a_live_fault() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut command = Command::new("sleep");
    command.arg("60");
    let mut child = ChildGuard::spawn("owned fault observation", command).unwrap();
    let status = child.fault_and_reap().unwrap();
    assert_eq!(status.signal(), Some(9));
    assert_eq!(child.exit_status().unwrap(), Some(status));
    assert!(child.fault_and_reap().is_err());
}

#[test]
fn sigstruct_parser_requires_exact_measurements_and_versions() {
    let parsed = parse_test_sgx_measurement(&format!(
        "mr_enclave: 0x{}\nmr_signer: {}\nisv_prod_id: 7\nisv_svn: 0x0008\n",
        "ab".repeat(32),
        "cd".repeat(32)
    ))
    .unwrap();
    assert_eq!(parsed.mrenclave, "ab".repeat(32));
    assert_eq!(parsed.mrsigner, "cd".repeat(32));
    assert_eq!(parsed.isv_prod_id, 7);
    assert_eq!(parsed.isv_svn, 8);
    assert!(parse_test_sgx_measurement("mr_enclave: 00\n").is_err());
}

#[test]
fn docker_image_identity_accepts_only_a_canonical_sha256_id() {
    let digest = format!("sha256:{}\n", "ab".repeat(32));
    let identity = DockerImageId::from_inspect_output(&digest).expect("canonical Docker image ID");
    assert_eq!(identity.as_str(), digest.trim());

    for invalid in [
        TEST_ENCLAVE_IMAGE.to_owned(),
        format!("sha256:{}", "ab".repeat(31)),
        format!("sha256:{}", "AB".repeat(32)),
        format!("sha256:{}z", "ab".repeat(31)),
    ] {
        assert!(
            DockerImageId::from_inspect_output(&invalid).is_err(),
            "accepted non-canonical Docker image identity: {invalid}"
        );
    }
}

#[test]
fn test_enclave_image_build_disables_nondeterministic_provenance() {
    assert_eq!(
        TEST_ENCLAVE_IMAGE_BUILD_ARGS,
        ["build", "--provenance=false", "-f"]
    );
}

#[test]
fn existing_signing_key_reuses_the_scenario_pinned_image_id() {
    let root = tempfile::tempdir().expect("temporary signing-key directory");
    let signing_key = root.path().join("test-sgx-signing-key.pem");
    fs::write(&signing_key, b"scenario signing key").expect("write signing key fixture");
    fs::set_permissions(&signing_key, fs::Permissions::from_mode(0o600))
        .expect("secure signing key permissions");
    let established = DockerImageId::from_inspect_output(&format!("sha256:{}", "ef".repeat(32)))
        .expect("established image ID");

    let resolved = ensure_enclave_image(
        Path::new("/unused/repository"),
        false,
        &signing_key,
        Some(&established),
    )
    .expect("reuse established image without resolving the mutable tag");

    assert_eq!(resolved, established);
}

#[test]
fn enclave_command_uses_the_pinned_image_id_instead_of_the_mutable_tag() {
    let root = tempfile::tempdir().expect("temporary enclave command inputs");
    let enclave_bin = root.path().join("outbe-tee-enclave");
    let signing_key = root.path().join("signing-key.pem");
    fs::write(&enclave_bin, b"binary").expect("write enclave binary fixture");
    fs::write(&signing_key, b"key").expect("write signing key fixture");
    let image_id = DockerImageId::from_inspect_output(&format!("sha256:{}", "cd".repeat(32)))
        .expect("pinned image ID");
    let spec = EnclaveSpec {
        name: "validator-0-tee".to_owned(),
        tee_port: 19500,
        enclave_bin,
        signing_key,
        network_descriptor: None,
        dev_network_binding: Some("01".repeat(66)),
        launch: EnclaveLaunch::Gramine {
            image_id: image_id.clone(),
        },
        sudo: false,
        pass_sgx_devices: false,
        remote_attestation: TestRemoteAttestation::None,
        dkg_seed: None,
        seal: None,
        log_path: root.path().join("enclave.log"),
        debug: false,
    };

    let command = build_enclave_command(&spec, &image_id).expect("build enclave command");
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(arguments
        .iter()
        .any(|argument| argument == image_id.as_str()));
    assert!(arguments
        .windows(2)
        .any(|pair| { pair[0] == "--dev-network-binding" && pair[1] == "01".repeat(66) }));
    assert!(!arguments
        .iter()
        .any(|argument| argument == TEST_ENCLAVE_IMAGE));
    let remote_attestation = arguments
        .windows(2)
        .position(|pair| pair[0] == "--env" && pair[1] == "OUTBE_TEST_REMOTE_ATTESTATION=none")
        .expect("Docker must pass the selected attestation mode into the container");
    let image = arguments
        .iter()
        .position(|argument| argument == image_id.as_str())
        .expect("pinned image ID argument");
    assert!(remote_attestation < image);
    assert!(command
        .get_envs()
        .all(|(key, _)| key != "OUTBE_TEST_REMOTE_ATTESTATION"));
    assert!(!arguments.iter().any(|argument| argument.contains("/qvl/")));
}

/// The native profile executes the enclave binary itself. No docker, no
/// bind mounts, no image id - and the seal directory is the real host path
/// rather than the container's `/tee`.
#[test]
fn native_enclave_command_runs_the_host_binary_without_docker() {
    let root = tempfile::tempdir().expect("temporary enclave command inputs");
    let enclave_bin = root.path().join("outbe-tee-enclave-mock");
    fs::write(&enclave_bin, b"binary").expect("write enclave binary fixture");
    let tee_dir = root.path().join("tee");
    let spec = EnclaveSpec {
        name: "validator-0-tee".to_owned(),
        tee_port: 19501,
        enclave_bin: enclave_bin.clone(),
        signing_key: root.path().join("unused-signing-key.pem"),
        network_descriptor: None,
        dev_network_binding: Some("01".repeat(66)),
        launch: EnclaveLaunch::NativeHost,
        sudo: true,
        pass_sgx_devices: false,
        remote_attestation: TestRemoteAttestation::None,
        dkg_seed: Some("ab".repeat(32)),
        seal: Some(SealSpec {
            tee_dir: tee_dir.clone(),
            chain_id_hex: format!("0x{:064x}", 424_242),
        }),
        log_path: root.path().join("enclave.log"),
        debug: false,
    };

    let command = build_native_command(&spec).expect("build native enclave command");
    assert_eq!(command.get_program(), enclave_bin.as_os_str());
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(arguments.iter().filter(|a| *a == "-v").count(), 0);
    assert!(!arguments.iter().any(|a| a == "run" || a == "--network"));
    assert!(arguments
        .windows(2)
        .any(|pair| pair[0] == "--socket" && pair[1] == "127.0.0.1:19501"));
    assert!(arguments
        .windows(2)
        .any(|pair| { pair[0] == "--dev-network-binding" && pair[1] == "01".repeat(66) }));
    // The container's `/tee` would be meaningless to a host process: the
    // sealed offer key has to land in this validator's own directory.
    let canonical = tee_dir.canonicalize().expect("seal dir created by builder");
    assert!(arguments
        .windows(2)
        .any(|pair| pair[0] == "--tee-dir" && pair[1] == canonical.display().to_string()));
    assert!(arguments.windows(2).any(|pair| pair[0] == "--chain-id"));
}

/// `sudo` is a docker concern. A native enclave must never be launched
/// through it, whatever the run-wide flag says.
#[test]
fn native_enclave_command_never_shells_through_sudo() {
    let root = tempfile::tempdir().expect("temporary enclave command inputs");
    let enclave_bin = root.path().join("outbe-tee-enclave-mock");
    fs::write(&enclave_bin, b"binary").expect("write enclave binary fixture");
    let spec = EnclaveSpec {
        name: "validator-0-tee".to_owned(),
        tee_port: 19502,
        enclave_bin,
        signing_key: root.path().join("unused-signing-key.pem"),
        network_descriptor: None,
        dev_network_binding: None,
        launch: EnclaveLaunch::NativeHost,
        sudo: true,
        pass_sgx_devices: false,
        remote_attestation: TestRemoteAttestation::None,
        dkg_seed: None,
        seal: None,
        log_path: root.path().join("enclave.log"),
        debug: false,
    };

    let command = build_native_command(&spec).expect("build native enclave command");
    assert_ne!(command.get_program(), "sudo");
    assert_ne!(command.get_program(), "docker");
}

/// A re-bootstrap can orphan the previous run's enclave. Attaching a node to
/// it would surface much later as an offer-key divergence crash, so the
/// native profile refuses to start on top of a port somebody already owns.
#[test]
fn native_enclave_refuses_a_port_an_earlier_run_still_owns() {
    let Ok(held) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
        // Some restricted test sandboxes deny loopback bind entirely.
        return;
    };
    let port = held.local_addr().expect("bound port").port();
    let root = tempfile::tempdir().expect("temporary enclave command inputs");
    let enclave_bin = root.path().join("outbe-tee-enclave-mock");
    fs::write(&enclave_bin, b"binary").expect("write enclave binary fixture");
    let spec = EnclaveSpec {
        name: "validator-0-tee".to_owned(),
        tee_port: port,
        enclave_bin,
        signing_key: root.path().join("unused-signing-key.pem"),
        network_descriptor: None,
        dev_network_binding: None,
        launch: EnclaveLaunch::NativeHost,
        sudo: false,
        pass_sgx_devices: false,
        remote_attestation: TestRemoteAttestation::None,
        dkg_seed: None,
        seal: None,
        log_path: root.path().join("enclave.log"),
        debug: false,
    };

    let error = spawn_enclave(spec).expect_err("occupied TEE port must fail closed");
    let message = error.to_string();
    assert!(message.contains(&port.to_string()), "{message}");
    assert!(message.contains("localnet stop"), "{message}");
}

#[test]
fn only_explicit_dcap_remote_attestation_requires_qvl() {
    assert!(!TestRemoteAttestation::None.requires_qvl());
    assert!(TestRemoteAttestation::Dcap.requires_qvl());
}

#[test]
fn dcap_test_renderers_bind_the_pinned_container_qvl_directory() {
    const ENTRYPOINT: &str =
        include_str!("../../../../../bin/outbe-tee-enclave/gramine/entrypoint.test.sh");
    const INSPECTOR: &str =
        include_str!("../../../../../bin/outbe-tee-enclave/gramine/inspect-test-measurement.sh");

    for (name, renderer) in [("entrypoint", ENTRYPOINT), ("inspector", INSPECTOR)] {
        assert_eq!(
            renderer.matches("-Dqvl_host_dir=/qvl").count(),
            1,
            "{name} must bind the already pinned /qvl directory into the DCAP manifest"
        );
    }
}

#[test]
fn first_hex_runs() {
    assert_eq!(
        first_hex("pub: abcdef0123", 6),
        Some("abcdef0123".to_string())
    );
    assert_eq!(first_hex("0xDEAD 12", 4), Some("DEAD".to_string()));
    assert_eq!(first_hex("short ab", 4), None);
}

#[test]
fn args_stringifies_display_tokens() {
    let port: u16 = 8545;
    let path = PathBuf::from("/tmp/x/data");
    let a = args!["node", "--http.port", port, "--datadir", path.display()];
    assert_eq!(
        a,
        vec!["node", "--http.port", "8545", "--datadir", "/tmp/x/data"]
    );
}

#[test]
fn diagnostic_argv_redacts_secret_values_in_both_supported_forms() {
    let argv = vec![
        "node".to_owned(),
        "--private-key".to_owned(),
        "secret-a".to_owned(),
        "--p2p-secret-key-hex=secret-b".to_owned(),
        "--dkg-seed".to_owned(),
        "secret-c".to_owned(),
        "--rpc-url".to_owned(),
        "http://127.0.0.1:8545".to_owned(),
    ];
    let redacted = redact_args_for_log(&argv);
    assert_eq!(
        redacted,
        vec![
            "node",
            "--private-key",
            "<redacted>",
            "--p2p-secret-key-hex=<redacted>",
            "--dkg-seed",
            "<redacted>",
            "--rpc-url",
            "http://127.0.0.1:8545",
        ]
    );
    let rendered = redacted.join(" ");
    for secret in ["secret-a", "secret-b", "secret-c"] {
        assert!(!rendered.contains(secret), "leaked {secret}");
    }
}

#[test]
fn real_mode_fails_closed_without_an_sgx_enclave_device() {
    assert!(select_sgx_enclave_device(true, false, false).is_err());
    assert_eq!(
        select_sgx_enclave_device(false, true, true).unwrap(),
        None,
        "gramine-direct must not inherit host SGX devices"
    );
}

#[test]
fn real_sgx_manifest_bounds_threads_for_four_validator_e2e() {
    let manifest = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../bin/outbe-tee-enclave/gramine/outbe-tee-enclave.manifest.template"
    ));
    let max_threads = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("sgx.max_threads = "))
        .and_then(|value| value.parse::<u32>().ok())
        .expect("manifest declares numeric sgx.max_threads");
    assert!(
        max_threads <= 16,
        "four real enclaves must not reserve more than 64 SGX thread slots"
    );
}
