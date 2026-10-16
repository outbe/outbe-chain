use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use crate::env::Environment;
use crate::internal::config::Config;
use crate::internal::proc::{ChildGuard, DockerImageId};

use super::{quiesce_and_terminate_committee_with, start_prepared_cohort, Localnet};

#[test]
fn founder_cohort_waits_for_all_authenticated_preparations_before_launch() {
    let mut prepared = Vec::new();
    let mut elapsed = 0;
    let mut launched = Vec::new();
    let mut state = (&mut prepared, &mut elapsed, &mut launched);
    start_prepared_cohort(
        &mut state,
        &[0, 1, 2, 3],
        |(prepared, elapsed, _), index| {
            **elapsed += if index == 0 { 2 } else { 79 };
            prepared.push(index);
            Ok(())
        },
        |(prepared, elapsed, launched), index| {
            assert_eq!(prepared.as_slice(), &[0, 1, 2, 3]);
            assert_eq!(
                **elapsed, 239,
                "cold setup must be outside every node's timer"
            );
            launched.push(index);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(launched, [0, 1, 2, 3]);
}

#[test]
fn founder_cohort_preparation_failure_never_launches_a_partial_committee() {
    for failed_index in 0..4 {
        let mut launched = Vec::new();
        let result = start_prepared_cohort(
            &mut launched,
            &[0, 1, 2, 3],
            |_, index| {
                eyre::ensure!(
                    index != failed_index,
                    "injected authenticated setup failure"
                );
                Ok(())
            },
            |launched, index| {
                launched.push(index);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(launched.is_empty());
    }
}

#[test]
fn founder_cohort_checks_reconnect_identity_and_requires_no_offer_key() {
    use alloy_primitives::B256;
    use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, NodeIdV1};
    use outbe_tee::protocol::EnclaveResponse;

    let manifest = EnclaveInitializationManifestV1 {
        chain_id: B256::repeat_byte(1).0,
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::GramineDirectDev,
        node_id: NodeIdV1 {
            reth_p2p_public: [3; 33],
        },
        initialization_challenge: [4; 32],
        node_host_noise_x25519: [5; 32],
        recipient_x25519: [6; 32],
        attestation_ed25519: [7; 32],
        noise_responder_x25519: [8; 32],
    };
    for mutation in 0..5 {
        let response = EnclaveResponse::PublicKeys {
            offer_key_ready: mutation == 1,
            recipient_x25519_pub: if mutation == 2 {
                [9; 32]
            } else {
                manifest.recipient_x25519
            },
            attestation_pub: if mutation == 3 {
                [9; 32]
            } else {
                manifest.attestation_ed25519
            },
            noise_static_pub: if mutation == 4 {
                [9; 32]
            } else {
                manifest.noise_responder_x25519
            },
            tee_bls_pub: Vec::new(),
            dkg_enc_pub: [10; 32],
            dkg_enc_sig: Vec::new(),
        };
        assert_eq!(
            super::verify_pre_dkg_public_identity(response, &manifest).is_ok(),
            mutation == 0
        );
    }
}

#[test]
fn founder_preparation_creates_node_datadir_before_authenticated_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        data_dir: directory.path().to_path_buf(),
        ..Environment::default()
    };
    let cfg = Config::resolve(&env);
    let data = cfg.validator_dir(0).join("data");
    assert!(!data.exists());
    let localnet = Localnet::new(cfg);
    let error = localnet.prepare_fresh_founder_node_host(0).unwrap_err();
    assert!(format!("{error:#}").contains("read founder's provisioned Reth P2P signer"));
    assert!(
        data.is_dir(),
        "the NodeHost API owns only its private child; the harness must create the node datadir first"
    );
    assert!(
        !data
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .exists(),
        "missing credentials must not initialize NodeHost state"
    );
}

#[test]
fn founder_preparation_rejects_a_file_instead_of_node_datadir_without_overwriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        data_dir: directory.path().to_path_buf(),
        ..Environment::default()
    };
    let cfg = Config::resolve(&env);
    std::fs::create_dir_all(cfg.validator_dir(0)).unwrap();
    let data = cfg.validator_dir(0).join("data");
    std::fs::write(&data, b"not a directory").unwrap();
    let localnet = Localnet::new(cfg);
    assert!(localnet.prepare_fresh_founder_node_host(0).is_err());
    assert_eq!(std::fs::read(data).unwrap(), b"not a directory");
}

#[test]
fn founder_preparation_does_not_touch_existing_restart_state() {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        tee_mode: crate::env::TeeMode::SgxNoAttest,
        data_dir: directory.path().to_path_buf(),
        ..Environment::default()
    };
    let cfg = Config::resolve(&env);
    let state = cfg
        .validator_dir(0)
        .join("data")
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("sentinel"), b"existing state").unwrap();
    let localnet = Localnet::new(cfg);
    // No endpoint or signer exists: touching recovery would fail here.
    localnet.prepare_fresh_founder_node_host(0).unwrap();
    assert_eq!(
        std::fs::read(state.join("sentinel")).unwrap(),
        b"existing state"
    );
    assert_eq!(std::fs::read_dir(state).unwrap().count(), 1);
}

#[test]
fn controlled_time_restart_resumes_the_frozen_cohort_for_graceful_shutdown() {
    let pids = [11, 22, 33, 44];
    let mut delivered = Vec::new();

    quiesce_and_terminate_committee_with(&pids, |actual_pids, signal| {
        assert_eq!(actual_pids, pids);
        delivered.push(signal.to_owned());
        Ok(())
    })
    .unwrap();

    assert_eq!(delivered, ["STOP", "TERM", "CONT"]);
}

#[test]
fn controlled_time_restart_resumes_the_cohort_when_term_delivery_fails() {
    let pids = [11, 22, 33, 44];
    let mut delivered = Vec::new();

    let error = quiesce_and_terminate_committee_with(&pids, |actual_pids, signal| {
        assert_eq!(actual_pids, pids);
        delivered.push(signal.to_owned());
        if signal == "TERM" {
            eyre::bail!("injected TERM failure");
        }
        Ok(())
    })
    .expect_err("TERM failure must abort the controlled-time restart");

    assert!(error.to_string().contains("injected TERM failure"));
    assert_eq!(delivered, ["STOP", "TERM", "CONT"]);
}

#[test]
fn readiness_rejects_an_owned_validator_that_exited() {
    let env = Environment {
        validators: 1,
        data_dir: tempfile::tempdir().unwrap().keep(),
        ..Environment::default()
    };
    env.ports.start_scenario(1).unwrap();
    let mut localnet = Localnet::new(Config::resolve(&env));

    let mut command = Command::new("sh");
    command.args(["-c", "exit 23"]);
    let guard = ChildGuard::spawn("validator-0-test", command).unwrap();
    localnet.validators.insert(0, guard);
    sleep(Duration::from_millis(50));

    let error = localnet
        .ensure_committee_alive()
        .expect_err("an exited owned validator must fail readiness");
    assert!(error.to_string().contains("validator-0 exited"));
}

fn owned_restart_fixture() -> (tempfile::TempDir, Localnet, (u32, u32)) {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        validators: 1,
        data_dir: directory.path().to_path_buf(),
        ..Environment::default()
    };
    env.ports.start_scenario(1).unwrap();
    let mut localnet = Localnet::new(Config::resolve(&env));
    let mut node = Command::new("sleep");
    node.arg("60");
    let node = ChildGuard::spawn("owned-restart-node", node).unwrap();
    let mut enclave = Command::new("sleep");
    enclave.arg("60");
    let enclave = crate::internal::proc::EnclaveGuard::from_test_child(
        ChildGuard::spawn("owned-restart-enclave", enclave).unwrap(),
    );
    let pids = (node.pid(), enclave.pid());
    localnet.validators.insert(0, node);
    localnet.enclaves.insert(0, enclave);
    (directory, localnet, pids)
}

#[test]
fn kill_validator_preserves_another_runs_same_slot_and_the_enclave() {
    let (_directory, mut localnet, pids) = owned_restart_fixture();
    let mut command = Command::new("bash");
    command.args([
        "-c",
        "exec -a \"$1\" sleep 60",
        "fixture",
        "outbe-chain node --datadir /another-run/validator-0/data",
    ]);
    let mut survivor = ChildGuard::spawn("other-run-validator-0", command).unwrap();
    // Let bash replace itself so the child has the formerly matched argv.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observed = Command::new("ps")
            .args(["-p", &survivor.pid().to_string(), "-o", "args="])
            .output()
            .unwrap();
        assert!(
            observed.status.success(),
            "owned fixture must still be alive"
        );
        let argv = String::from_utf8(observed.stdout).unwrap();
        if argv
            .trim_start()
            .starts_with("outbe-chain node --datadir /another-run/validator-0/data ")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        sleep(Duration::from_millis(10));
    }
    localnet.kill_validator(0).unwrap();
    let stopped = Command::new("ps")
        .args(["-p", &pids.0.to_string(), "-o", "pid="])
        .output()
        .unwrap();
    assert!(!stopped.status.success() && stopped.stdout.is_empty());
    assert!(!localnet.validators.contains_key(&0));
    assert_eq!(localnet.enclaves.get(&0).unwrap().pid(), pids.1);
    assert!(localnet
        .enclaves
        .get_mut(&0)
        .unwrap()
        .exit_status()
        .unwrap()
        .is_none());
    assert!(survivor.exit_status().unwrap().is_none());
    assert!(localnet
        .kill_validator(0)
        .unwrap_err()
        .to_string()
        .contains("no owned stop target"));
    assert!(survivor.exit_status().unwrap().is_none());
}

#[test]
fn owned_restart_rejects_either_changed_identity_without_stopping_children() {
    for change_node in [true, false] {
        let (_directory, mut localnet, pids) = owned_restart_fixture();
        let expected = if change_node {
            (0, pids.1)
        } else {
            (pids.0, 0)
        };
        let mut observed = false;
        let result = localnet.restart_validator_and_enclave_owned_observed(
            0,
            expected.0,
            expected.1,
            |_| {
                observed = true;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!observed);
        assert_eq!(localnet.live_validator_and_enclave_pids(0).unwrap(), pids);
    }
}

#[test]
fn owned_restart_arms_observation_after_reaping_and_preserves_other_children() {
    let (_directory, mut localnet, pids) = owned_restart_fixture();
    let mut survivor = Command::new("sleep");
    survivor.arg("60");
    let survivor = ChildGuard::spawn("owned-restart-survivor", survivor).unwrap();
    let survivor_pid = survivor.pid();
    localnet.validators.insert(1, survivor);
    let mut observed = false;
    let result = localnet.restart_validator_and_enclave_owned_observed(0, pids.0, pids.1, |net| {
        assert!(!net.validators.contains_key(&0));
        assert!(!net.enclaves.contains_key(&0));
        assert_eq!(net.validator_pid(1)?, survivor_pid);
        observed = true;
        Err(eyre::eyre!("injected observation failure"))
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("injected observation failure"));
    assert!(observed);
    assert!(localnet
        .validators
        .get_mut(&1)
        .unwrap()
        .exit_status()
        .unwrap()
        .is_none());
    assert!(!localnet.validators.contains_key(&0));
    assert!(!localnet.enclaves.contains_key(&0));
}

#[test]
fn owned_restart_rejects_an_already_exited_node_without_replacing_its_enclave() {
    let (_directory, mut localnet, pids) = owned_restart_fixture();
    localnet
        .validators
        .get_mut(&0)
        .unwrap()
        .stop_and_reap()
        .unwrap();
    let mut observed = false;
    let result = localnet.restart_validator_and_enclave_owned_observed(0, pids.0, pids.1, |_| {
        observed = true;
        Ok(())
    });
    assert!(result.is_err());
    assert!(!observed);
    assert_eq!(localnet.live_enclave_pid(0).unwrap(), pids.1);
}

#[test]
fn pinned_enclave_image_is_not_resolved_again_on_committee_restart() {
    let env = Environment {
        validators: 1,
        data_dir: tempfile::tempdir().unwrap().keep(),
        ..Environment::default()
    };
    env.ports.start_scenario(1).unwrap();
    let mut localnet = Localnet::new(Config::resolve(&env));
    let mut resolutions = 0;
    let expected = format!("sha256:{}", "1".repeat(64));

    localnet
        .ensure_enclave_image_once_with(|| {
            resolutions += 1;
            DockerImageId::from_inspect_output(&expected)
        })
        .unwrap();
    localnet
        .ensure_enclave_image_once_with(|| {
            resolutions += 1;
            DockerImageId::from_inspect_output(&format!("sha256:{}", "2".repeat(64)))
        })
        .unwrap();

    assert_eq!(resolutions, 1);
    assert_eq!(localnet.enclave_image_id(), Some(expected.as_str()));
}
