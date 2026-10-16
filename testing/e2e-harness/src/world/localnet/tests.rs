use super::*;
use crate::env::Environment;
use crate::world::state::DkgExpiryExpectedExit;
use std::ffi::OsStr;

#[test]
fn node_ipc_preflight_rejects_dynamic_node_before_spawning() {
    let env = Environment::default();
    let mut localnet = Localnet::new(Config::resolve(&env));
    let node_dir = PathBuf::from("/tmp").join("long-node-directory-".repeat(10));
    let error = localnet
        .spawn_node(
            "unstarted-follower",
            15,
            &node_dir,
            Command::new("/nonexistent/e2e-node"),
        )
        .expect_err("invalid IPC path must precede executable lookup");
    assert!(error.to_string().contains("invalid node IPC socket path"));
    assert!(error.to_string().contains(&node_dir.display().to_string()));
}

fn launch_log_fixture(localnet: &mut Localnet, index: usize, marker: &str) -> u32 {
    use std::time::{Duration, Instant};

    let node_dir = localnet.cfg.validator_dir(index);
    fs::create_dir_all(&node_dir).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "printf '%s\\n' \"$1\"", "node-log-fixture", marker]);
    proc::attach_log(&mut command, &node_dir).unwrap();
    let mut child = localnet
        .spawn_node("node-log-fixture", index, &node_dir, command)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.exit_status().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "log fixture did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = child.pid();
    localnet.validators.insert(index, child);
    pid
}

#[test]
fn owned_node_cleanup_rejects_silent_failure_and_checks_expected_slot() {
    for (code, expected_slots, accepted) in [
        (0, vec![], true),
        (1, vec![], false),
        (1, vec![0], true),
        (1, vec![1], false),
        (2, vec![0], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let node_dir = localnet.cfg.validator_dir(0);
        fs::create_dir_all(&node_dir).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", &format!("exit {code}")]);
        proc::attach_log(&mut command, &node_dir).unwrap();
        let mut child = localnet
            .spawn_node("exit-fixture", 0, &node_dir, command)
            .unwrap();
        child.reap_fault(std::time::Duration::from_secs(2)).unwrap();
        localnet.validators.insert(0, child);
        assert_eq!(
            localnet.stop_owned_nodes(&expected_slots, &[]).is_ok(),
            accepted,
            "exit {code}, expected {expected_slots:?}"
        );
        assert!(localnet.validators.is_empty());
        assert!(localnet.node_launch_logs.is_empty());
    }
}

fn expiry_cleanup_fixture(script: &str, exited: bool) -> (tempfile::TempDir, Localnet, u32) {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment {
        data_dir: dir.path().canonicalize().unwrap(),
        ..Environment::default()
    };
    let mut localnet = Localnet::new(Config::resolve(&env));
    let node_dir = localnet.cfg.validator_dir(0);
    fs::create_dir_all(&node_dir).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    proc::attach_log(&mut command, &node_dir).unwrap();
    // attach_log defaults stdin to null. This fixture must keep `read`
    // blocked until cleanup signals it instead of exiting on immediate EOF.
    command.stdin(std::process::Stdio::piped());
    let mut child = localnet
        .spawn_node("expiry-exit-fixture", 0, &node_dir, command)
        .unwrap();
    if exited {
        child.reap_fault(std::time::Duration::from_secs(2)).unwrap();
    } else {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !localnet
            .node_launch_log(0, child.pid())
            .unwrap()
            .contains("ready\n")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "live fixture not ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(child.exit_status().unwrap().is_none());
    }
    let pid = child.pid();
    localnet.validators.insert(0, child);
    (dir, localnet, pid)
}

#[test]
fn dkg_expiry_cleanup_requires_exact_owned_and_launch_pid() {
    for damage in [
        "none",
        "witness_pid",
        "zero_pid",
        "witness_slot",
        "launch_pid",
        "missing_launch",
        "missing_owner",
        "missing_proof",
    ] {
        let (_dir, mut localnet, pid) = expiry_cleanup_fixture("exit 1", true);
        let mut proofs = vec![DkgExpiryExpectedExit {
            slot: 0,
            node_pid: pid,
        }];
        match damage {
            "none" => {}
            "witness_pid" => proofs[0].node_pid += 1,
            "zero_pid" => proofs[0].node_pid = 0,
            "witness_slot" => proofs[0].slot = 1,
            "launch_pid" => localnet.node_launch_logs.get_mut(&0).unwrap().0 += 1,
            "missing_launch" => {
                localnet.node_launch_logs.remove(&0);
            }
            "missing_owner" => {
                localnet.validators.remove(&0);
            }
            "missing_proof" => proofs.clear(),
            _ => unreachable!(),
        }
        assert_eq!(
            localnet.stop_owned_nodes(&[], &proofs).is_ok(),
            damage == "none",
            "accepted damaged DKG expiry evidence: {damage}"
        );
        assert!(localnet.validators.is_empty());
        assert!(localnet.node_launch_logs.is_empty());
    }
}

#[test]
fn dkg_expiry_cleanup_rejects_success_other_errors_signals_and_live_children() {
    for (script, exited, accepted) in [
        ("exit 1", true, true),
        ("exit 0", true, false),
        ("exit 2", true, false),
        ("kill -KILL $$", true, false),
        // Readiness is emitted after installing the trap. Cleanup itself
        // will produce exit 1, which must not count as a natural halt.
        (
            "trap 'exit 1' TERM; printf 'ready\\n'; read unused",
            false,
            false,
        ),
    ] {
        let (_dir, mut localnet, pid) = expiry_cleanup_fixture(script, exited);
        let proof = DkgExpiryExpectedExit {
            slot: 0,
            node_pid: pid,
        };
        // Even an existing slot allowance cannot bypass the exact proof.
        assert_eq!(
            localnet.stop_owned_nodes(&[0], &[proof]).is_ok(),
            accepted,
            "script {script}, exited before cleanup {exited}"
        );
        assert!(localnet.validators.is_empty());
    }
}

#[test]
fn node_launch_log_excludes_prior_incarnation_and_rejects_stale_pid() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment {
        data_dir: dir.path().canonicalize().unwrap(),
        ..Environment::default()
    };
    let mut localnet = Localnet::new(Config::resolve(&env));
    let node_dir = localnet.cfg.validator_dir(0);
    fs::create_dir_all(&node_dir).unwrap();
    fs::write(node_dir.join("node.log"), "earlier matching marker\n").unwrap();
    let first = launch_log_fixture(&mut localnet, 0, "first launch");
    assert_eq!(
        localnet.node_launch_log(0, first).unwrap(),
        "first launch\n"
    );
    localnet.validators.remove(&0);
    let second = launch_log_fixture(&mut localnet, 0, "second launch");
    assert_eq!(
        localnet.node_launch_log(0, second).unwrap(),
        "second launch\n"
    );
    assert!(localnet.node_launch_log(0, first).is_err());
    assert!(localnet.node_launch_log(1, second).is_err());
}

#[test]
fn node_launch_log_rejects_missing_replaced_and_truncated_log() {
    for damage in ["missing", "replaced", "truncated"] {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let pid = launch_log_fixture(&mut localnet, 0, "current matching marker");
        assert_eq!(
            localnet.node_launch_log(0, pid).unwrap(),
            "current matching marker\n"
        );
        let path = localnet.cfg.validator_dir(0).join("node.log");
        match damage {
            "missing" => fs::remove_file(&path).unwrap(),
            "replaced" => {
                fs::rename(&path, path.with_extension("previous")).unwrap();
                fs::write(&path, "current matching marker\n").unwrap();
            }
            "truncated" => fs::write(&path, "").unwrap(),
            _ => unreachable!(),
        }
        assert!(localnet.node_launch_log(0, pid).is_err(), "{damage}");
    }
}

#[test]
fn node_launch_log_failed_spawn_discards_previous_capture() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment {
        data_dir: dir.path().canonicalize().unwrap(),
        ..Environment::default()
    };
    let mut localnet = Localnet::new(Config::resolve(&env));
    let pid = launch_log_fixture(&mut localnet, 0, "previous launch");
    localnet.validators.remove(&0);
    let node_dir = localnet.cfg.validator_dir(0);
    assert!(localnet
        .spawn_node(
            "missing-node",
            0,
            &node_dir,
            Command::new(dir.path().join("nonexistent-node"))
        )
        .is_err());
    assert!(localnet.node_launch_log(0, pid).is_err());
    assert!(localnet.node_launch_logs.is_empty());
}

#[test]
fn node_launch_log_cleanup_drops_capture_but_retains_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment {
        data_dir: dir.path().canonicalize().unwrap(),
        ..Environment::default()
    };
    let mut localnet = Localnet::new(Config::resolve(&env));
    let pid = launch_log_fixture(&mut localnet, 0, "retained diagnostics");
    localnet.clear_owned_nodes();
    assert!(localnet.node_launch_logs.is_empty());
    assert!(localnet.validators.is_empty());
    assert!(localnet.node_launch_log(0, pid).is_err());
    assert_eq!(
        fs::read_to_string(localnet.cfg.validator_dir(0).join("node.log")).unwrap(),
        "retained diagnostics\n"
    );
}

#[test]
fn node_ipc_preflight_rejects_bootstrap_before_files_or_subprocesses() {
    let directory = tempfile::tempdir().expect("test directory");
    let env = Environment {
        data_dir: directory.path().join("long-scenario-directory-".repeat(6)),
        seed: directory.path().join("nonexistent-seed.json"),
        chain_bin: directory.path().join("nonexistent-node"),
        ..Environment::default()
    };
    let localnet = Localnet::new(Config::resolve(&env));
    let error = localnet
        .bootstrap_with_profile(4, &BootstrapProfile::default())
        .expect_err("path validation must precede seed reads and bootstrap");
    assert!(error.to_string().contains("invalid node IPC socket path"));
    assert!(
        !env.data_dir.exists(),
        "failed preflight must not create scenario state"
    );
}

fn configured_timeout(mode: crate::env::TeeMode) -> Option<String> {
    let mut cmd = Command::new("outbe-chain");
    extend_real_sgx_process_environment(mode, &mut cmd);
    cmd.get_envs()
        .find(|(key, _)| *key == OsStr::new("OUTBE_TEE_IO_TIMEOUT_SECS"))
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())
}

#[test]
fn co_located_hardware_lane_alone_widens_enclave_io_timeout() {
    use crate::env::TeeMode;

    let expected = Some(crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string());
    assert_eq!(configured_timeout(TeeMode::Real), expected);
    assert_eq!(configured_timeout(TeeMode::SgxNoAttest), expected);
    assert_eq!(configured_timeout(TeeMode::Mock), None);
    assert_eq!(configured_timeout(TeeMode::GramineDirect), None);
}

#[test]
fn enclave_startup_budget_is_profile_aware_and_never_extends_scenario() {
    use crate::env::TeeMode;
    use std::time::{Duration, Instant};
    for mode in [
        TeeMode::Real,
        TeeMode::SgxNoAttest,
        TeeMode::GramineDirect,
        TeeMode::Mock,
        TeeMode::MockNative,
    ] {
        let env = Environment {
            tee_mode: mode,
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        for seconds in [10, 20] {
            let before = Instant::now();
            let deadline = localnet.enclave_startup_deadline(seconds);
            let budget = if mode.passes_sgx_devices() {
                crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
            } else {
                seconds
            };
            assert!(deadline >= before + Duration::from_secs(budget));
            assert!(deadline <= Instant::now() + Duration::from_secs(budget));
        }
        let outer = Instant::now() + Duration::from_secs(21);
        localnet.set_scenario_deadline(outer);
        assert_eq!(
            localnet.enclave_startup_deadline(20),
            outer - Duration::from_secs(20)
        );
    }
}

#[test]
fn sgx_no_attest_selects_production_node_host_session() {
    let env = Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        ..Environment::default()
    };
    env.ports
        .start_scenario(env.validators)
        .expect("allocate deterministic scenario ports");
    let localnet = Localnet::new(Config::for_scenario(&env, 1));
    let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);

    assert!(args
        .windows(2)
        .any(|pair| { pair[0] == "--tee-session-mode" && pair[1] == "production-node-host" }));
}

#[test]
fn sgx_no_attest_uses_the_hardware_bootstrap_allowance() {
    let env = Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        ..Environment::default()
    };
    env.ports
        .start_scenario(env.validators)
        .expect("allocate deterministic scenario ports");
    let localnet = Localnet::new(Config::for_scenario(&env, 1));

    assert_eq!(localnet.tee_bootstrap_wait_attempts(), 372);
    let mut args = Vec::new();
    localnet.extend_real_sgx_startup_timeout(&mut args);
    let bootstrap_timeout = args[1]
        .parse::<u64>()
        .expect("numeric hardware-SGX bootstrap timeout");
    let io_timeout = configured_timeout(crate::env::TeeMode::SgxNoAttest)
        .expect("hardware-SGX enclave I/O override")
        .parse::<u64>()
        .expect("numeric hardware-SGX enclave I/O timeout");
    assert_eq!(
        args,
        vec![
            "--tee-bootstrap-timeout-secs".to_owned(),
            crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string()
        ],
        "the node deadline must stay inside the harness observation envelope"
    );
    assert!(
        io_timeout >= bootstrap_timeout,
        "an individual SGX request must not expire before the enclosing bootstrap budget"
    );
}

#[test]
fn co_located_devnet_bounds_each_nodes_cross_block_cache() {
    let env = Environment::default();
    env.ports
        .start_scenario(env.validators)
        .expect("allocate deterministic scenario ports");
    let localnet = Localnet::new(Config::for_scenario(&env, 1));
    let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);
    let cache_size = args
        .windows(2)
        .find(|pair| pair[0] == "--engine.cross-block-cache-size")
        .map(|pair| pair[1].as_str());

    assert_eq!(cache_size, Some("512"));
}

#[test]
fn every_localnet_node_retains_the_native_state_proof_window() {
    let env = Environment::default();
    env.ports
        .start_scenario(env.validators)
        .expect("allocate deterministic scenario ports");
    let localnet = Localnet::new(Config::for_scenario(&env, 1));
    let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);
    let proof_window = args
        .windows(2)
        .find(|pair| pair[0] == "--rpc.eth-proof-window")
        .map(|pair| pair[1].as_str());

    assert_eq!(proof_window, Some("1868"));
}

#[test]
fn runtime_audit_stdout_is_free_of_ansi_formatting() {
    let env = Environment::default();
    env.ports
        .start_scenario(env.validators)
        .expect("allocate deterministic scenario ports");
    let localnet = Localnet::new(Config::for_scenario(&env, 1));
    let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);

    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--color" && pair[1] == "never"));
}

#[test]
fn generated_node_commands_reject_removed_tee_renewal_key_options() {
    let ordinary = vec!["node".to_owned(), "--validator".to_owned()];
    assert!(ensure_manual_tee_lease_node_args(&ordinary).is_ok());

    for removed in [
        "--node-evm-key",
        "--node-evm-key=/tmp/evm-key.hex",
        "--tee-renewal.relay-key",
        "--tee-renewal.rpc-url=http://127.0.0.1:8545",
        "--tee-renewal.poll-secs",
        "--tee-renewal.warning-blocks",
        "--tee-renewal.critical-blocks",
    ] {
        assert!(
            ensure_manual_tee_lease_node_args(&[removed.to_owned()]).is_err(),
            "removed option still accepted: {removed}"
        );
    }
}

/// Both layouts, and nothing else - in particular not `validator-*/data`.
#[test]
fn sealed_dirs_finds_only_enclave_mounts() {
    let root = std::env::temp_dir().join(format!("outbe-sealed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    // run-dir shape
    fs::create_dir_all(root.join("scenario-1/validator-0/tee")).expect("mk");
    fs::create_dir_all(root.join("scenario-1/validator-0/data")).expect("mk");
    // scenario-dir shape (a Localnet wiping its own dir)
    fs::create_dir_all(root.join("validator-7/tee")).expect("mk");
    fs::create_dir_all(root.join("validator-7/logs")).expect("mk");

    let mut found = sealed_dirs(&root);
    found.sort();
    assert_eq!(
        found,
        vec![
            root.join("scenario-1/validator-0/tee"),
            root.join("validator-7/tee"),
        ]
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn worldwide_day_is_eight_digits() {
    assert_eq!(ymd_utc(0), "19700101");
    let wd = worldwide_day();
    assert_eq!(wd.len(), 8);
    assert!(wd.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn custom_day_boundary_lead_is_reflected_exactly_in_the_node_clock_offset() {
    const NOW: u64 = 1_700_000_000;
    const LEAD: u64 = 240;
    use outbe_primitives::time::SECONDS_PER_DAY;

    let opts = StartOpts::near_next_utc_day_with_lead(6, NOW, LEAD);
    let next_day = NOW - (NOW % SECONDS_PER_DAY) + SECONDS_PER_DAY;

    assert_eq!(opts.voting_window, Some(6));
    assert_eq!(
        opts.unix_time_offset_secs,
        Some((next_day - LEAD) as i64 - NOW as i64)
    );
    assert!(!opts.genesis_timestamp_pre_shifted);
    assert!(!opts.is_txpool_eviction_profile);
}

#[test]
fn shortened_txpool_policy_is_an_explicit_scenario_profile() {
    let ordinary = StartOpts::with_voting_window(6);
    let eviction = StartOpts::with_txpool_eviction_profile(6);

    assert!(!ordinary.is_txpool_eviction_profile);
    assert!(eviction.is_txpool_eviction_profile);
    assert_eq!(eviction.voting_window, Some(6));
    assert_eq!(eviction.unix_time_offset_secs, None);
}
