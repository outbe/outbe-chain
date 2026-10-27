use super::cold_catchup::{finish_cold_catchup, require_cold_admission_checkpoint};
use super::*;

#[test]
fn cold_catchup_retains_primary_and_all_cleanup_failures() {
    for failures in 0..8 {
        let outcome = |bit, message| {
            if failures & bit != 0 {
                Err(eyre!("{message}"))
            } else {
                Ok(())
            }
        };
        let result = finish_cold_catchup(
            outcome(1, "catchup checkpoint timeout"),
            outcome(2, "shutdown timeout"),
            outcome(4, "log seal failed"),
        );
        if failures == 0 {
            result.expect("authority may start after complete success");
            continue;
        }
        let error = result.expect_err("any failure must prevent authority startup");
        let report = format!("{error:#}");
        for (bit, message) in [
            (1, "catchup checkpoint timeout"),
            (2, "shutdown timeout"),
            (4, "log seal failed"),
        ] {
            assert_eq!(report.contains(message), failures & bit != 0, "{report}");
        }
        if failures & 1 != 0 {
            assert_eq!(error.root_cause().to_string(), "catchup checkpoint timeout");
        }
    }
}

#[test]
fn registration_receipt_requires_exact_successful_mined_transaction() {
    let transaction = format!("{:#x}", B256::repeat_byte(1));
    let receipt = serde_json::json!({
        "transactionHash": transaction,
        "status": "0x1", "blockNumber": "0x2",
        "blockHash": format!("{:#x}", B256::repeat_byte(2)),
    });
    assert_eq!(
        registration_receipt(&transaction, receipt.clone())
            .unwrap()
            .block_number(),
        Some(2)
    );
    for (field, value) in [
        (
            "transactionHash",
            serde_json::json!(format!("{:#x}", B256::repeat_byte(3))),
        ),
        ("status", serde_json::json!("0x0")),
        ("status", serde_json::Value::Null),
        ("blockNumber", serde_json::Value::Null),
        ("blockHash", serde_json::Value::Null),
    ] {
        let mut bad = receipt.clone();
        bad[field] = value;
        assert!(registration_receipt(&transaction, bad).is_err());
    }
}

#[test]
fn joiner_extra_clock_is_rejected_before_runtime_directory_creation() {
    let root = tempfile::tempdir().unwrap();
    let env = crate::env::Environment::default();
    let mut cfg = crate::internal::config::Config::resolve(&env);
    cfg.dir = root.path().to_owned();
    let mut net = Localnet::new(cfg);
    for offset in [None, Some(0), Some(-12)] {
        net.start_opts.unix_time_offset_secs = offset;
        for extra in [
            vec!["--testnet.unix-time-offset-secs", "12"],
            vec!["--testnet.unix-time-offset-secs=12"],
        ] {
            assert!(net
                .joiner_validator_args(4, &extra)
                .unwrap_err()
                .to_string()
                .contains("shared StartOpts"));
            assert!(!net.cfg.validator_dir(4).exists());
        }
    }
}

#[test]
fn full_node_role_stop_requires_exact_live_owner_and_reaps_it() {
    let root = tempfile::tempdir().unwrap();
    let env = crate::env::Environment::default();
    let mut cfg = crate::internal::config::Config::resolve(&env);
    cfg.dir = root.path().to_owned();
    let mut net = Localnet::new(cfg);
    for index in [4, 5, 6] {
        // Model a node's signal handler, not sleep's default SIGTERM exit.
        // The marker is published only after the handler is installed.
        let ready = root.path().join(format!("ready-{index}"));
        let exit_code = if index == 6 { 1 } else { 0 };
        let mut command = Command::new("sh");
        command.args(["-c", &format!("trap 'exit {exit_code}' TERM INT; printf ready > \"$1\"; while :; do sleep 0.05; done"), "node-stop-fixture"]);
        command.arg(&ready);
        let name = Localnet::joiner_full_node_name(index);
        net.followers.insert(
            name.clone(),
            proc::ChildGuard::spawn(&name, command).unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while fs::read_to_string(&ready).ok().as_deref() != Some("ready") {
            assert!(
                std::time::Instant::now() < deadline,
                "signal handler not ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let original = net.owned_full_node_process(4).unwrap();
    assert!(net.stop_joiner_full_node_owned(4, 0).is_err());
    assert_eq!(net.owned_full_node_process(4).unwrap(), original);
    net.stop_joiner_full_node_owned(4, original.0).unwrap();
    assert!(net.owned_full_node_process(4).is_err());
    assert!(net.owned_full_node_process(5).unwrap().1.is_none());
    net.followers
        .get_mut(&Localnet::joiner_full_node_name(5))
        .unwrap()
        .stop_and_reap()
        .unwrap();
    let exited = net.owned_full_node_process(5).unwrap();
    assert!(exited.1.is_some());
    assert!(net.stop_joiner_full_node_owned(5, exited.0).is_err());
    let failed = net.owned_full_node_process(6).unwrap();
    assert!(net.stop_joiner_full_node_owned(6, failed.0).is_err());
}

#[test]
fn lease_full_node_releases_only_proven_exited_ownership_with_logs_retained() {
    use crate::internal::launch_log::LaunchLog;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let env = crate::env::Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        ..crate::env::Environment::default()
    };
    let mut cfg = crate::internal::config::Config::resolve(&env);
    cfg.dir = root.path().to_owned();
    let mut localnet = Localnet::new(cfg);
    let index = 4;
    let vd = localnet.cfg.validator_dir(index);
    fs::create_dir_all(&vd).unwrap();
    let mut log = LaunchLog::arm(&vd.join("node.log")).unwrap();
    let name = Localnet::joiner_full_node_name(index);
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'controlled exit\\n'", "fixture", "--datadir"]);
    command.arg(vd.join("data"));
    attach_log(&mut command, &vd).unwrap();
    let child = proc::ChildGuard::spawn(&name, command).unwrap();
    let pid = child.pid();
    localnet.followers.insert(name.clone(), child);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (observed_pid, exit) = localnet.owned_full_node_process(index).unwrap();
        assert_eq!(observed_pid, pid);
        if let Some(exit) = exit {
            assert!(exit.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    log.seal().unwrap();
    let proof = log.read().unwrap();
    assert_eq!(proof, "controlled exit\n");
    assert!(localnet
        .wait_selected_upstream_admission(index, 0)
        .unwrap_err()
        .to_string()
        .contains("still owned"));
    localnet.stop_follower(&name).unwrap();
    assert!(localnet.owned_full_node_process(index).is_err());
    let error = localnet
        .wait_selected_upstream_admission(index, 0)
        .unwrap_err();
    assert!(!error.to_string().contains("still owned"), "{error:#}");
    assert_eq!(log.read().unwrap(), proof);

    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec sleep 30", "fixture", "--datadir"]);
    command.arg(vd.join("data"));
    let child = proc::ChildGuard::spawn(&name, command).unwrap();
    localnet.followers.insert(name, child);
    assert!(localnet.owned_full_node_process(index).unwrap().1.is_none());
    assert!(localnet
        .wait_selected_upstream_admission(index, 0)
        .unwrap_err()
        .to_string()
        .contains("still owned"));
}

#[test]
fn cold_joiner_follower_preserves_provisioned_paths_and_protocol_options() {
    let root = tempfile::tempdir().unwrap();
    let env = crate::env::Environment::default();
    env.ports.start_scenario(env.validators).unwrap();
    let mut cfg = crate::internal::config::Config::for_scenario(&env, 1);
    cfg.dir = root.path().to_owned();
    let mut localnet = Localnet::new(cfg);
    localnet.start_opts.unix_time_offset_secs = Some(123);
    fs::write(
        localnet.cfg.dir.join("validators.json"),
        r#"[{"public_key":"fixture-peer","p2p_address":"127.0.0.1:9000"}]"#,
    )
    .unwrap();
    let vd = localnet.cfg.validator_dir(4);
    fs::create_dir_all(&vd).unwrap();
    fs::write(vd.join("reth-p2p-secret.hex"), "11".repeat(32)).unwrap();
    for offset in [None, Some(0), Some(123), Some(-37)] {
        localnet.start_opts.unix_time_offset_secs = offset;
        let argv = localnet
            .joiner_validator_args(4, &["--consensus.keys-dir", "/exact/custom-keys"])
            .unwrap();
        assert!(argv.iter().any(|arg| arg == "--validator"));
        assert!(argv.windows(2).any(|pair| pair
            == [
                "--upstream".to_owned(),
                format!("http://127.0.0.1:{}", localnet.cfg.http_port(0)),
            ]));
        for removed in ["--consensus.public-polynomial", "--consensus.dkg-output"] {
            assert!(!argv.iter().any(|arg| arg == removed));
        }
        let inherited = argv
            .iter()
            .filter(|arg| arg.starts_with("--testnet.unix-time-offset-secs"))
            .cloned()
            .collect::<Vec<_>>();
        let expected = offset
            .into_iter()
            .map(|value| format!("--testnet.unix-time-offset-secs={value}"))
            .collect::<Vec<_>>();
        assert_eq!(inherited, expected);
        let follower = super::super::follower::derive_validator_recovery_follower_args(
            &argv,
            "http://127.0.0.1:8545",
        )
        .unwrap();
        assert_eq!(
            follower
                .iter()
                .filter(|arg| arg.starts_with("--testnet.unix-time-offset-secs"))
                .cloned()
                .collect::<Vec<_>>(),
            expected
        );
    }
    localnet.start_opts.unix_time_offset_secs = Some(123);
    let argv = localnet
        .joiner_validator_args(4, &["--consensus.keys-dir", "/exact/custom-keys"])
        .unwrap();
    let follower = super::super::follower::derive_validator_recovery_follower_args(
        &argv,
        "http://127.0.0.1:8545",
    )
    .unwrap();
    for option in [
        "--datadir",
        "--chain",
        "--p2p-secret-key",
        "--tee-enclave-socket",
        "--consensus.keys-dir",
    ] {
        let original = argv.windows(2).find(|pair| pair[0] == option).unwrap();
        assert!(
            follower.windows(2).any(|pair| pair == original),
            "lost {option}"
        );
    }
    assert!(follower.contains(&"--testnet.unix-time-offset-secs=123".to_owned()));
    assert!(!follower.contains(&"--validator".to_owned()));
    assert!(!follower.contains(&"--validator.evm-key".to_owned()));
    assert_eq!(
        fs::read_to_string(vd.join("reth-p2p-secret.hex")).unwrap(),
        "11".repeat(32)
    );
}

#[test]
fn cold_joiner_requires_exact_admission_height_hash_and_root() {
    let anchor = outbe_tee::FinalizedJoinAdmissionAnchorV1 {
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(1),
        node_id_hash: B256::repeat_byte(2),
        enclave_id: B256::repeat_byte(3),
        intent_hash: B256::repeat_byte(4),
        finalized_height: 19,
        finalized_hash: B256::repeat_byte(5),
        finalized_state_root: B256::repeat_byte(6),
        finalized_consensus_timestamp: 20,
    };
    let exact = crate::world::rpc::FinalizedCheckpoint {
        height: 19,
        block_hash: anchor.finalized_hash,
        state_root: anchor.finalized_state_root,
    };
    require_cold_admission_checkpoint(anchor, exact).unwrap();
    for wrong in [
        crate::world::rpc::FinalizedCheckpoint {
            height: 18,
            ..exact
        },
        crate::world::rpc::FinalizedCheckpoint {
            block_hash: B256::ZERO,
            ..exact
        },
        crate::world::rpc::FinalizedCheckpoint {
            state_root: B256::ZERO,
            ..exact
        },
    ] {
        assert!(require_cold_admission_checkpoint(anchor, wrong).is_err());
    }
}

#[test]
fn real_sgx_offer_read_retries_only_bounded_io_timeouts() {
    assert!(retry_node_offer_read(
        true,
        0,
        &TransportError::IoTimeout {
            operation: "Noise handshake response read",
            timeout_secs: 30,
        }
    ));
    assert!(retry_node_offer_read(
        true,
        1,
        &TransportError::IoTimeout {
            operation: "request response read",
            timeout_secs: 30,
        }
    ));
    assert!(!retry_node_offer_read(
        true,
        2,
        &TransportError::IoTimeout {
            operation: "request response read",
            timeout_secs: 30,
        }
    ));
    assert!(!retry_node_offer_read(
        false,
        0,
        &TransportError::IoTimeout {
            operation: "Noise handshake response read",
            timeout_secs: 30,
        }
    ));
    assert!(!retry_node_offer_read(
        true,
        0,
        &TransportError::Handshake(
            "wrapper mentioned enclave io timeout but this is policy".to_owned()
        )
    ));
}

#[test]
fn manual_renewal_cli_result_is_checked_before_reading_the_journal() {
    assert_eq!(
        classify_manual_renewal_cli_output(
            "Finalized {\n    finalized_height: 42,\n    valid_until: 99,\n}"
        ),
        ManualRenewalCliOutcomeV1::Finalized
    );
    assert_eq!(
        classify_manual_renewal_cli_output(
            "NotDue {\n    finalized_height: 41,\n    opens_at_timestamp: 98,\n}"
        ),
        ManualRenewalCliOutcomeV1::NotDue
    );
    assert_eq!(
        classify_manual_renewal_cli_output("Submitted { transaction_hash: 0x01 }"),
        ManualRenewalCliOutcomeV1::Unexpected
    );
}

#[test]
fn full_node_runtime_does_not_load_separately_provisioned_role_keys() {
    let args = full_node_joiner_role_args(
        "p2p-secret",
        "127.0.0.1:34000",
        "http://127.0.0.1:35000",
        "127.0.0.1:36000",
    );

    for forbidden in [
        "--validator",
        "--consensus.signing-key",
        "--validator.evm-key",
    ] {
        assert!(!args.iter().any(|arg| arg == forbidden), "{forbidden}");
    }
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--upstream", "http://127.0.0.1:35000"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--tee-enclave-socket", "127.0.0.1:34000"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--consensus.listen-addr", "127.0.0.1:36000"]));
}

#[test]
fn downgrade_log_requires_the_exact_production_no_attest_rejection() {
    assert!(has_exact_production_no_attest_rejection(
        "production DCAP release refuses runtime attestation none (gramine-sgx; remote attestation disabled - EGETKEY sealing available)"
    ));
    assert!(!has_exact_production_no_attest_rejection(
        "runtime mode does not satisfy DcapRequired"
    ));
    assert!(!has_exact_production_no_attest_rejection(
        "production DCAP release refuses runtime attestation sgx-no-attest"
    ));
}
