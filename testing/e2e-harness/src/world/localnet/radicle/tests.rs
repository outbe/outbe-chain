use std::{
    fs,
    io::Read as _,
    os::unix::fs::{symlink, MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::Duration,
};

use super::readiness::{
    native_address, native_node_id, provisioned_radicle_node_id, wait_radicle_ready,
    RadicleReadiness,
};
use super::source::{
    configure_user_radicle, connected_sessions, native_sessions, pushed_commit_visible,
    repository_visible, require_public_source_repository, seed_scope_all, source_control_response,
    wait_for_source_seeds, InventoryPublication,
};
use super::{
    block_on, decode_repo_id, materialized_max_validators, prepare_private_socket_parent,
    query_sidecar, verify_user_tool_version, write_outbe_profile, ChildGuard,
    RadicleRepositoryFixtureV1, NATIVE_CONTROL_DEADLINE,
};

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn readiness_child() -> ChildGuard {
    let mut command = Command::new("sleep");
    command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ChildGuard::spawn("radicle-readiness-fixture", command).unwrap()
}

#[test]
fn readiness_public_identity_needs_no_private_key_or_profile() {
    let root = tempfile::tempdir().unwrap();
    assert!(provisioned_radicle_node_id(root.path()).is_err());
    let keys = root.path().join("keys");
    fs::create_dir(&keys).unwrap();
    let public = keys.join("radicle.pub");
    fs::write(&public, "malformed public fixture").unwrap();
    assert!(provisioned_radicle_node_id(root.path()).is_err());
    // OpenSSH encoding of public bytes [7; 32], not a generated secret.
    fs::write(&public, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH fixture\n").unwrap();
    assert_eq!(provisioned_radicle_node_id(root.path()).unwrap(), [7; 32]);
    assert!(!keys.join("radicle").exists());
    assert!(!root.path().join("config.json").exists());
    fs::rename(&public, keys.join("public-fixture")).unwrap();
    symlink(keys.join("public-fixture"), &public).unwrap();
    assert!(provisioned_radicle_node_id(root.path()).is_err());
}

#[test]
fn readiness_accepts_owned_live_native_identity_without_changing_endpoints() {
    for peer_port in [8776, 9776] {
        let peer: std::net::SocketAddr = ([127, 0, 0, 1], peer_port).into();
        let status = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let status_addr = status.local_addr().unwrap();
        let (root, socket, server) = native_control_fixture(vec![
            serde_json::json!(native_node_id([7; 32])),
            serde_json::json!({"externalAddresses": [peer.to_string()]}),
        ]);
        let mut guard = readiness_child();
        let pid = guard.pid();
        wait_radicle_ready(
            &mut guard,
            &RadicleReadiness {
                expected_node_id: [7; 32],
                control_socket: &socket,
                peer,
                status: status_addr,
                log: &root.path().join("radicle.log"),
            },
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(guard.pid(), pid);
        assert!(guard.exit_status().unwrap().is_none());
        assert_eq!(status.local_addr().unwrap(), status_addr);
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(!root.path().join("config.json").exists());
        assert!(!root.path().join("keys").exists());
    }
}

#[test]
fn readiness_foreign_status_listener_cannot_replace_missing_native_control() {
    let root = tempfile::tempdir().unwrap();
    let status = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = root.path().join("missing.sock");
    let log = root.path().join("radicle.log");
    let mut guard = readiness_child();
    let error = wait_radicle_ready(
        &mut guard,
        &RadicleReadiness {
            expected_node_id: [7; 32],
            control_socket: &socket,
            peer: ([127, 0, 0, 1], 8776).into(),
            status: status.local_addr().unwrap(),
            log: &log,
        },
        Duration::from_millis(50),
    )
    .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("native control observation failed"));
    assert!(diagnostic.contains(&format!("pid={}", guard.pid())));
    assert!(diagnostic.contains(&socket.display().to_string()));
    assert!(diagnostic.contains(&log.display().to_string()));
    assert!(guard.exit_status().unwrap().is_none());
    assert!(std::net::TcpStream::connect(status.local_addr().unwrap()).is_ok());
}

#[test]
fn readiness_rejects_wrong_native_identity_and_advertised_listen_endpoint() {
    for (nid, addresses, expected_error) in [
        (
            native_node_id([8; 32]),
            vec!["127.0.0.1:8776"],
            "NodeId differs",
        ),
        (
            native_node_id([7; 32]),
            vec!["127.0.0.1:9776"],
            "advertised addresses differ",
        ),
        (
            native_node_id([7; 32]),
            vec!["127.0.0.1:8776", "127.0.0.1:9776"],
            "advertised addresses differ",
        ),
    ] {
        let (root, socket, server) = native_control_fixture(vec![
            serde_json::json!(nid),
            serde_json::json!({"externalAddresses": addresses}),
        ]);
        let status = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut guard = readiness_child();
        let error = wait_radicle_ready(
            &mut guard,
            &RadicleReadiness {
                expected_node_id: [7; 32],
                control_socket: &socket,
                peer: ([127, 0, 0, 1], 8776).into(),
                status: status.local_addr().unwrap(),
                log: &root.path().join("radicle.log"),
            },
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(expected_error));
        server.join().unwrap();
        assert!(guard.exit_status().unwrap().is_none());
    }
}

#[test]
fn readiness_exited_owner_fails_even_with_a_foreign_status_listener() {
    let root = tempfile::tempdir().unwrap();
    let status = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 7"]);
    let mut guard = ChildGuard::spawn("exited-radicle-fixture", command).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while guard.exit_status().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline);
        sleep(Duration::from_millis(1));
    }
    let error = wait_radicle_ready(
        &mut guard,
        &RadicleReadiness {
            expected_node_id: [7; 32],
            control_socket: &root.path().join("control.sock"),
            peer: ([127, 0, 0, 1], 8776).into(),
            status: status.local_addr().unwrap(),
            log: &root.path().join("radicle.log"),
        },
        Duration::from_secs(2),
    )
    .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("owned Radicle child exited"));
    assert!(diagnostic.contains("exit status: 7"));
    assert_eq!(guard.exit_status().unwrap().unwrap().code(), Some(7));
    assert!(std::net::TcpStream::connect(status.local_addr().unwrap()).is_ok());
}

#[test]
fn readiness_native_identity_does_not_replace_status_tcp() {
    let (root, socket, server) = native_control_fixture(vec![
        serde_json::json!(native_node_id([7; 32])),
        serde_json::json!({"externalAddresses": ["127.0.0.1:8776"]}),
    ]);
    let mut guard = readiness_child();
    // Port zero cannot be a listening TCP endpoint. This negative fixture
    // avoids a release/rebind race because it does not select an ephemeral
    // port.
    assert!(wait_radicle_ready(
        &mut guard,
        &RadicleReadiness {
            expected_node_id: [7; 32],
            control_socket: &socket,
            peer: ([127, 0, 0, 1], 8776).into(),
            status: ([127, 0, 0, 1], 0).into(),
            log: &root.path().join("radicle.log"),
        },
        Duration::from_millis(100),
    )
    .is_err());
    server.join().unwrap();
    assert!(guard.exit_status().unwrap().is_none());
}

#[test]
fn native_observations_need_no_profile_and_follow_endpoint_replacement() {
    let nid = native_node_id([7; 32]);
    let peer = native_node_id([8; 32]);
    let (root, socket, server) = native_control_fixture(vec![
        serde_json::json!(nid),
        serde_json::json!({"externalAddresses": ["127.0.0.1:8776"]}),
        serde_json::json!(nid),
        serde_json::json!({"externalAddresses": ["127.0.0.1:9776"]}),
        serde_json::json!([
            {"nid": peer, "addr": "127.0.0.1:8777", "link": "outbound", "state": {"connected": {}}},
            {"nid": nid, "addr": "127.0.0.1:9776", "link": "inbound", "state": {"disconnected": {}}}
        ]),
    ]);
    let before = block_on(query_sidecar(socket.clone(), NATIVE_CONTROL_DEADLINE)).unwrap();
    let after = block_on(query_sidecar(socket.clone(), NATIVE_CONTROL_DEADLINE)).unwrap();
    assert_eq!(before.node_id, after.node_id);
    assert_eq!(
        native_address(&before.addresses[0]).unwrap(),
        "127.0.0.1:8776"
    );
    assert_eq!(
        native_address(&after.addresses[0]).unwrap(),
        "127.0.0.1:9776"
    );
    assert_eq!(
        connected_sessions(native_sessions(socket).unwrap()).unwrap(),
        vec![(peer, "127.0.0.1:8777".to_owned())]
    );
    let commands = server.join().unwrap();
    assert_eq!(
        commands,
        vec![
            serde_json::json!({"command": "nodeId"}),
            serde_json::json!({"command": "config"}),
            serde_json::json!({"command": "nodeId"}),
            serde_json::json!({"command": "config"}),
            serde_json::json!({"command": "sessions"}),
        ]
    );
    assert!(!root.path().join("config.json").exists());
    assert!(!root.path().join("keys").exists());
}

#[test]
fn native_session_errors_are_not_an_empty_mesh() {
    for response in [
        serde_json::json!({"error": "sidecar unavailable"}),
        serde_json::json!([{"nid": "invalid"}]),
    ] {
        let (_root, socket, server) = native_control_fixture(vec![response]);
        assert!(native_sessions(socket).is_err());
        server.join().unwrap();
    }
}

#[test]
fn native_user_setup_publishes_inventory_and_discovers_live_validator_seeds() {
    let nid = native_node_id([7; 32]);
    let repo = [5; 20];
    for result in [
        serde_json::json!({"status": "connected"}),
        serde_json::json!({"status": "disconnected", "reason": "rejected"}),
    ] {
        let success = result["status"] == "connected";
        let mut responses = vec![serde_json::json!({"updated": true}), result];
        if success {
            responses.extend([
                serde_json::json!({"updated": true}),
                serde_json::json!([{
                    "nid": nid, "addrs": [],
                    "state": {"connected": {"since": 0}}
                }]),
            ]);
        }
        let (_user_root, user, user_server) = native_control_fixture(responses);
        let (_validator_root, validator, validator_server) = native_control_fixture(vec![
            serde_json::json!(nid),
            serde_json::json!({"externalAddresses": ["127.0.0.1:9776"]}),
        ]);
        assert_eq!(
            configure_user_radicle(user, repo, vec![validator]).is_ok(),
            success
        );
        let commands = user_server.join().unwrap();
        assert_eq!(
            commands[0],
            serde_json::json!({
                "command": "seed", "scope": "all",
                "rid": format!("rad:{}", multibase::encode(multibase::Base::Base58Btc, repo)),
            })
        );
        assert_eq!(commands[1]["command"], "connect");
        assert_eq!(commands[1]["addr"], format!("{nid}@127.0.0.1:9776"));
        assert_eq!(commands[1]["opts"]["persistent"], false);
        if success {
            assert_eq!(commands[2]["command"], "addInventory");
            assert_eq!(commands[2]["rid"], commands[0]["rid"]);
            assert_eq!(commands[3]["command"], "seedsFor");
            assert_eq!(commands[3]["rid"], commands[0]["rid"]);
        }
        assert_eq!(
            validator_server.join().unwrap(),
            vec![
                serde_json::json!({"command": "nodeId"}),
                serde_json::json!({"command": "config"}),
            ]
        );
    }
}

#[test]
fn native_user_publication_is_idempotent_but_errors_are_not_acknowledgements() {
    let nid = native_node_id([7; 32]);
    for publication in [
        serde_json::json!({}),
        serde_json::json!({"updated": false}),
        serde_json::json!({"error": "inventory rejected"}),
    ] {
        let rejected = publication.get("error").is_some();
        let mut responses = vec![
            serde_json::json!({"updated": false}),
            serde_json::json!({"status": "connected"}),
            publication,
        ];
        if !rejected {
            responses.push(serde_json::json!([{
                "nid": nid, "addrs": [], "state": {"connected": {"since": 0}}
            }]));
        }
        let (_user_root, user, user_server) = native_control_fixture(responses);
        let (_validator_root, validator, validator_server) = native_control_fixture(vec![
            serde_json::json!(nid),
            serde_json::json!({"externalAddresses": ["127.0.0.1:9776"]}),
        ]);
        let result = configure_user_radicle(user, [5; 20], vec![validator]);
        if rejected {
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains("inventory rejected"), "{error}");
            assert!(
                error.contains("publish independent source inventory"),
                "{error}"
            );
        } else {
            result.unwrap();
        }
        assert_eq!(
            user_server.join().unwrap().len(),
            if rejected { 3 } else { 4 }
        );
        assert_eq!(validator_server.join().unwrap().len(), 2);
    }
}

#[test]
fn native_source_discovery_requires_every_expected_connected_seed() {
    let rid = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    let first = native_node_id([7; 32]);
    let second = native_node_id([8; 32]);
    let foreign = native_node_id([9; 32]);
    let (_root, socket, server) = native_control_fixture(vec![
        serde_json::json!([
            {"nid": first, "addrs": [], "state": {"connected": {"since": 0}}},
            {"nid": second, "addrs": []},
            {"nid": foreign, "addrs": [], "state": {"connected": {"since": 0}}}
        ]),
        serde_json::json!([
            {"nid": first, "addrs": [], "state": {"connected": {"since": 0}}},
            {"nid": second, "addrs": [], "state": {"connected": {"since": 0}}}
        ]),
    ]);
    wait_for_source_seeds(
        &socket,
        rid.parse().unwrap(),
        &[[7; 32], [8; 32]],
        Duration::from_secs(5),
    )
    .unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request["command"] == "seedsFor" && request["rid"] == rid));
}

#[test]
fn native_source_discovery_rejects_errors_empty_cohorts_and_elapsed_deadlines() {
    let rid = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    let (_root, socket, server) =
        native_control_fixture(vec![serde_json::json!({"error": "routing unavailable"})]);
    let error = wait_for_source_seeds(
        &socket,
        rid.parse().unwrap(),
        &[[7; 32]],
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("routing unavailable"));
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(
        wait_for_source_seeds(&socket, rid.parse().unwrap(), &[], Duration::from_secs(5)).is_err()
    );
    let error = wait_for_source_seeds(&socket, rid.parse().unwrap(), &[[7; 32]], Duration::ZERO)
        .unwrap_err();
    assert!(error.to_string().contains("discovery deadline"));
    assert!(error.to_string().contains(&native_node_id([7; 32])));
}

#[test]
fn source_control_decoded_success_after_deadline_is_rejected() {
    static DECODED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    struct SlowReply;
    impl<'de> serde::Deserialize<'de> for SlowReply {
        fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
            let _ = <bool as serde::Deserialize>::deserialize(decoder)?;
            DECODED.store(true, std::sync::atomic::Ordering::SeqCst);
            sleep(Duration::from_millis(150));
            Ok(Self)
        }
    }
    let (_root, socket, server) = native_control_fixture(vec![serde_json::json!(true)]);
    let result = source_control_response::<SlowReply>(
        &socket,
        radicle::node::Command::NodeId,
        std::time::Instant::now() + Duration::from_millis(100),
    );
    server.join().unwrap();
    assert!(DECODED.load(std::sync::atomic::Ordering::SeqCst));
    let error = result.err().expect("expired decoded success must fail");
    assert!(format!("{error:#}").contains("deadline exceeded"));
}

#[test]
fn source_control_stall_and_truncated_reply_fail_and_close_the_socket() {
    use std::io::{BufRead as _, Write as _};
    for truncated in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "missing request");
                        sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept source request: {error}"),
                }
            };
            // Keep the listener pollable and the accepted exchange blocking.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut request)
                .unwrap();
            if truncated {
                stream.write_all(b"{}").unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
            }
            // The client must drop the timed-out/incomplete exchange, not
            // leave a detached blocking operation waiting for this server.
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let rid = format!(
            "rad:{}",
            multibase::encode(multibase::Base::Base58Btc, [5; 20])
        );
        let result = source_control_response::<InventoryPublication>(
            &socket,
            radicle::node::Command::AddInventory {
                rid: rid.parse().unwrap(),
            },
            std::time::Instant::now() + Duration::from_millis(100),
        );
        server.join().unwrap();
        let error = format!("{:#}", result.err().expect("response must fail"));
        assert!(
            error.contains(if truncated {
                "response incomplete"
            } else {
                "deadline exceeded"
            }),
            "{error}"
        );
    }
}

#[test]
fn native_source_discovery_partial_response_cannot_extend_deadline() {
    use std::io::{BufRead as _, Write as _};
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "missing request");
                    sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept source request: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut request)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap()["command"],
            "seedsFor"
        );
        // Each byte arrives within the old per-read timeout, but the complete
        // valid response arrives after the discovery's absolute deadline.
        for _ in 0..20 {
            if stream.write_all(b" ").is_err() {
                return;
            }
            sleep(Duration::from_millis(20));
        }
        let response = serde_json::json!([{
            "nid": native_node_id([7; 32]), "addrs": [],
            "state": {"connected": {"since": 0}}
        }]);
        if serde_json::to_writer(&mut stream, &response).is_ok() {
            let _ = stream.write_all(b"\n");
        }
    });
    let rid = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    let result = wait_for_source_seeds(
        &socket,
        rid.parse().unwrap(),
        &[[7; 32]],
        Duration::from_millis(100),
    );
    server.join().unwrap();
    assert!(
        result.is_err(),
        "late partial response must not satisfy discovery"
    );
}

#[test]
fn native_endpoint_format_preserves_ipv6_and_dns() {
    use outbe_radicle::endpoint::EndpointAddress;
    let ipv6 = EndpointAddress::ipv6(std::net::Ipv6Addr::LOCALHOST.octets(), 8776).unwrap();
    assert_eq!(native_address(&ipv6).unwrap(), "[::1]:8776");
    let dns = EndpointAddress::dns("peer.example", 8777).unwrap();
    assert_eq!(native_address(&dns).unwrap(), "peer.example:8777");
}

#[test]
fn user_tools_must_match_the_chain_revision() {
    let revision = "a825e13a324fe1f10df486690a746cbd80da66d0";
    for name in ["rad", "git-remote-rad"] {
        assert!(
            verify_user_tool_version(name, &format!("{name} 1.0 (a825e13)\n"), revision).is_ok()
        );
        assert!(
            verify_user_tool_version(name, &format!("{name} 1.0 ({revision})"), revision).is_ok()
        );
        assert!(
            verify_user_tool_version(name, &format!("{name} 1.0 (b76a178)"), revision).is_err()
        );
        assert!(
            verify_user_tool_version(name, &format!("{name} 1.0 (deadbee)"), revision).is_err()
        );
        assert!(verify_user_tool_version(name, &format!("{name} 1.0 (a82)"), revision).is_err());
        assert!(
            verify_user_tool_version(name, &format!("{name} 1.0 (a825e13-dirty)"), revision)
                .is_err()
        );
    }
}

#[test]
fn seeding_observation_requires_exact_persisted_allow_all_without_a_profile() {
    use radicle::node::policy::{store::StoreWriter, Policy, Scope};
    let root = tempfile::tempdir().unwrap();
    let node = root.path().join("node");
    fs::create_dir(&node).unwrap();
    let database = node.join(radicle::node::POLICIES_DB_FILE);
    let repo = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    let other = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [6; 20])
    );
    let rid = repo.parse().unwrap();
    let mut writer = StoreWriter::open(&database).unwrap();
    writer.seed(&other.parse().unwrap(), Scope::All).unwrap();
    assert!(!seed_scope_all(root.path(), &repo).unwrap());
    writer.seed(&rid, Scope::Followed).unwrap();
    assert!(!seed_scope_all(root.path(), &repo).unwrap());
    writer.seed(&rid, Scope::All).unwrap();
    assert!(seed_scope_all(root.path(), &repo).unwrap());
    writer.set_seed_policy(&rid, Policy::Block).unwrap();
    assert!(!seed_scope_all(root.path(), &repo).unwrap());
    drop(writer);
    assert!(!seed_scope_all(root.path(), &repo).unwrap());
    assert!(!root.path().join("config.json").exists());
    assert!(!root.path().join("keys").exists());
}

#[test]
fn seeding_observation_preserves_missing_and_corrupt_store_errors() {
    let root = tempfile::tempdir().unwrap();
    let repo = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    assert!(seed_scope_all(root.path(), &repo).is_err());
    assert!(!root.path().join("node").exists());
    fs::create_dir(root.path().join("node")).unwrap();
    let database = root
        .path()
        .join("node")
        .join(radicle::node::POLICIES_DB_FILE);
    fs::write(&database, "not a sqlite database").unwrap();
    assert!(seed_scope_all(root.path(), &repo).is_err());
    assert_eq!(fs::read(&database).unwrap(), b"not a sqlite database");
}

#[test]
fn repository_absence_is_pending_but_corruption_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let canonical = multibase::encode(multibase::Base::Base58Btc, [5; 20]);
    let fixture = RadicleRepositoryFixtureV1 {
        home: root.path().join("source"),
        worktree: root.path().join("worktree"),
        repo_id: format!("rad:{canonical}"),
        repo_id_hex: hex::encode([5; 20]),
        issue_id: "11".repeat(20),
        patch_id: "22".repeat(20),
        pushed_commit: Some("33".repeat(20)),
    };
    assert!(!repository_visible(root.path(), &fixture).unwrap());
    assert!(require_public_source_repository(&fixture).is_err());
    assert!(!fixture.home.exists());
    assert!(!root.path().join("storage").exists());
    fs::create_dir_all(root.path().join("storage").join(canonical)).unwrap();
    assert!(repository_visible(root.path(), &fixture).is_err());
    assert!(!root.path().join("config.json").exists());
}

#[test]
fn pushed_commit_requires_the_exact_master_ref_and_commit_object() {
    use radicle::git::raw;
    let root = tempfile::tempdir().unwrap();
    let git = raw::Repository::init_bare(root.path()).unwrap();
    let tree_id = git.treebuilder(None).unwrap().write().unwrap();
    let tree = git.find_tree(tree_id).unwrap();
    let signature = raw::Signature::now("Fixture", "fixture@example.test").unwrap();
    let commit = git
        .commit(
            Some("refs/heads/master"),
            &signature,
            &signature,
            "replicated update",
            &tree,
            &[],
        )
        .unwrap();
    git.set_head("refs/heads/master").unwrap();
    let rid = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, [5; 20])
    );
    let repository =
        radicle::storage::git::Repository::open(root.path(), rid.parse().unwrap()).unwrap();
    assert!(pushed_commit_visible(&repository, &commit.to_string()).unwrap());
    assert!(!pushed_commit_visible(&repository, &"11".repeat(20)).unwrap());
    assert!(pushed_commit_visible(&repository, "not-an-oid").is_err());
    // The object still exists, but an unrelated branch must not prove the push.
    git.reference("refs/heads/other", commit, false, "fixture branch")
        .unwrap();
    git.set_head("refs/heads/other").unwrap();
    assert!(!pushed_commit_visible(&repository, &commit.to_string()).unwrap());
}

fn native_control_fixture(
    responses: Vec<serde_json::Value>,
) -> (
    tempfile::TempDir,
    PathBuf,
    std::thread::JoinHandle<Vec<serde_json::Value>>,
) {
    use std::io::{BufRead as _, Write as _};
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let mut commands = Vec::new();
        for response in responses {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing control request"
                        );
                        sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept native control request: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            commands.push(serde_json::from_str(&line).unwrap());
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        }
        commands
    });
    (root, socket, server)
}

#[test]
fn canonical_repo_id_decodes_to_registry_bytes() {
    let expected = [0x5a; 20];
    let canonical = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, expected)
    );
    assert_eq!(decode_repo_id(&canonical).unwrap(), expected);
    assert!(decode_repo_id("rad:f0011").is_err());
}

#[test]
fn materialized_genesis_drives_radicle_capacity() {
    let root = tempfile::tempdir().expect("create capacity fixture");
    let genesis = root.path().join("genesis.json");
    fs::write(
        &genesis,
        serde_json::to_vec(&serde_json::json!({
            "alloc": {
                "000000000000000000000000000000000000ee00": {
                    "storage": {
                        "0x0000000000000000000000000000000000000000000000000000000000000001":
                            "0x0000000000000000000000000000000000000000000000000000000000000080"
                    }
                }
            }
        }))
        .expect("encode genesis fixture"),
    )
    .expect("write genesis fixture");

    let max_validators = materialized_max_validators(&genesis).expect("read capacity");
    assert_eq!(max_validators, 128);
    write_outbe_profile(root.path(), "127.0.0.1:8776", max_validators).expect("write profile");
    let profile: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("config.json")).expect("read profile"))
            .expect("decode profile");
    assert_eq!(
        profile.pointer("/node/limits/connection/outbound"),
        Some(&serde_json::json!(127))
    );
    assert_eq!(
        profile.pointer("/node/limits/connection/inbound"),
        Some(&serde_json::json!(143))
    );
}

#[test]
fn short_socket_runtime_is_private_and_rejects_symlink_parents() {
    let fixture = tempfile::tempdir().expect("create runtime fixture");
    let runtime = fixture.path().join("runtime");
    let socket = runtime.join("scenario-1/v0.sock");
    prepare_private_socket_parent(&runtime, &socket, fixture.path())
        .expect("prepare private runtime");
    let expected_uid = fs::symlink_metadata(fixture.path())
        .expect("inspect fixture owner")
        .uid();

    for directory in [&runtime, socket.parent().unwrap()] {
        let metadata = fs::symlink_metadata(directory).expect("inspect private runtime");
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.uid(), expected_uid);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    let external = fixture.path().join("external");
    fs::create_dir(&external).expect("create external directory");
    let linked_runtime = fixture.path().join("linked-runtime");
    symlink(&external, &linked_runtime).expect("link runtime directory");
    let linked_socket = linked_runtime.join("scenario-1/v0.sock");
    assert!(
        prepare_private_socket_parent(&linked_runtime, &linked_socket, fixture.path()).is_err()
    );
}

#[test]
fn launcher_rejects_second_home_owner() {
    let fixture = launcher_fixture();
    let mut first = KillOnDrop(
        launcher_command(&fixture.home, &fixture.binary)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start first launcher"),
    );
    wait_for_launcher(&mut first.0, &fixture.home);

    let second = launcher_command(&fixture.home, &fixture.binary)
        .output()
        .expect("run competing launcher");
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("a live sidecar already owns"),
        "unexpected competing-launcher error: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn launcher_rejects_managed_directory_symlink() {
    let fixture = launcher_fixture();
    let external = fixture.root.path().join("external-storage");
    fs::create_dir(&external).expect("create external storage");
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700))
        .expect("protect external storage");
    symlink(&external, fixture.home.join("storage")).expect("link managed storage");

    let output = launcher_command(&fixture.home, &fixture.binary)
        .output()
        .expect("run launcher with symlink");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected non-symlink directory"),
        "unexpected symlink error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct LauncherFixture {
    root: tempfile::TempDir,
    home: PathBuf,
    binary: PathBuf,
}

fn launcher_fixture() -> LauncherFixture {
    let root = tempfile::tempdir().expect("create launcher fixture");
    let home = root.path().join("radicle");
    let keys = home.join("keys");
    fs::create_dir_all(&keys).expect("create key directory");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("protect home");
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).expect("protect keys");
    for name in ["radicle", "radicle.pub"] {
        let path = keys.join(name);
        fs::write(&path, "fixture\n").expect("write fixture key");
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if name == "radicle" { 0o600 } else { 0o644 }),
        )
        .expect("set fixture key mode");
    }
    let binary = root.path().join("outbe-radicle-fixture");
    fs::write(
        &binary,
        concat!(
            "#!/usr/bin/env python3\n",
            "import socket\n",
            "import sys\n",
            "path = sys.argv[sys.argv.index('--control-socket') + 1]\n",
            "listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n",
            "listener.bind(path)\n",
            "listener.listen()\n",
            "with open(path + '.ready', 'x', encoding='utf-8') as marker:\n",
            "    marker.write('ready\\n')\n",
            "while True:\n",
            "    connection, _ = listener.accept()\n",
            "    connection.close()\n",
        ),
    )
    .expect("write fake sidecar");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
        .expect("make fake sidecar executable");
    LauncherFixture { root, home, binary }
}

fn launcher_command(home: &Path, binary: &Path) -> Command {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/validator/run-radicle.sh");
    let mut command = Command::new(script);
    command
        .env("OUTBE_RADICLE_BINARY", binary)
        .env(
            "OUTBE_RADICLE_CONTROL_SOCKET",
            home.join("node/outbe-control.sock"),
        )
        .args([
            home.as_os_str(),
            "127.0.0.1:18776".as_ref(),
            "127.0.0.1:18777".as_ref(),
            "4".as_ref(),
            "127.0.0.1:18776".as_ref(),
        ]);
    command
}

/// Waits until the launcher has taken ownership of the home.
///
/// The managed directories are created just before the script execs the
/// sidecar, so their presence is the last observable step of setup. It
/// used to wait on `config.json`, but the launcher no longer writes one.
/// The sidecar builds its runtime config from its command line and never
/// reads that file.
fn wait_for_launcher(child: &mut Child, home: &Path) {
    let control_socket = home.join("node/outbe-control.sock");
    let readiness_marker = control_socket.with_extension("sock.ready");
    for _ in 0..100 {
        if let Some(status) = child.try_wait().expect("poll launcher") {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                pipe.read_to_string(&mut stderr)
                    .expect("read failed launcher stderr");
            }
            panic!("first launcher exited early with {status}: {stderr}");
        }
        if readiness_marker.is_file() {
            return;
        }
        sleep(Duration::from_millis(10));
    }
    panic!(
        "first launcher did not publish readiness for its control socket at {}",
        readiness_marker.display()
    );
}
