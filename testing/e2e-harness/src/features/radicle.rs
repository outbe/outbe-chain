//! Release Radicle integration scenario.

use std::{collections::BTreeSet, thread::sleep, time::Duration};

use cucumber::{given, then, when};

use crate::features::common::start_bootstrapped_localnet;
use crate::world::localnet::{
    worldwide_day, BootstrapProfile, RadicleRepositoryFixtureV1, StartOpts,
};
use crate::world::World;

const USER_KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
const RADICLE_DKG_EPOCH_BLOCKS: u64 = 60;
const RADICLE_DKG_PREPARE_BLOCKS: u64 = 15;
const RADICLE_DKG_ACTIVATION_GRACE_BLOCKS: u64 = 20;

#[given("a fresh four-validator Radicle localnet")]
fn fresh(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "Radicle release lane requires four validators"
    );
    let profile = BootstrapProfile::default()
        .with_dkg_timing(
            RADICLE_DKG_EPOCH_BLOCKS,
            RADICLE_DKG_PREPARE_BLOCKS,
            RADICLE_DKG_ACTIVATION_GRACE_BLOCKS,
        )
        .expect("valid short Radicle DKG schedule");
    world.state.voting_window = 6;
    world.state.wwd = Some(worldwide_day());
    world
        .localnet
        .bootstrap_with_profile(world.validators.size(), &profile)
        .expect("bootstrap Radicle localnet with checked DKG schedule");
    start_bootstrapped_localnet(world, &StartOpts::with_voting_window(6));
}

#[then("Radicle genesis and the signed validator mesh are ready")]
fn mesh_ready(world: &mut World) {
    assert_eq!(world.rpc.radicle_registry_state(), Some((0, 0, u32::MAX)));
    wait_until("four Ready Radicle validators", 120, || {
        (0..4).all(|index| {
            world
                .rpc
                .radicle_status(world.validators.http_port(index))
                .is_some_and(|status| status["phase"] == "ready")
        })
    });

    let primary = world.validators.primary_port();
    let genesis_hash = world.rpc.block_hash(primary, 0).expect("genesis hash");
    let finalized_height = world
        .rpc
        .finalized(primary)
        .expect("finalized binding height");
    let validator_node_ids = (0..4)
        .map(|index| {
            world
                .rpc
                .radicle_status(world.validators.http_port(index))
                .and_then(|status| status["localNodeId"].as_str().map(str::to_owned))
                .unwrap_or_else(|| panic!("validator-{index} has no Radicle NodeId"))
        })
        .collect::<Vec<_>>();
    let validator_native_node_ids = (0..4)
        .map(|index| {
            let addresses = world
                .localnet
                .validator_radicle_addresses(index)
                .expect("native Radicle advertised address");
            addresses[0]
                .split_once('@')
                .map(|(node_id, _)| node_id.to_owned())
                .expect("native Radicle address contains NodeId")
        })
        .collect::<Vec<_>>();
    let founder_validator_addresses = (0..4)
        .map(|index| {
            let key = world.validators.get(index).evm_key().expect("founder key");
            world.rpc.address_of(&key).expect("founder EVM address")
        })
        .collect::<Vec<_>>();
    let founder_onchain_node_ids = founder_validator_addresses
        .iter()
        .map(|address| {
            format!(
                "{:#x}",
                world
                    .rpc
                    .validator_radicle_node_id_at(primary, address, finalized_height)
                    .expect("finalized founder Radicle binding")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        founder_onchain_node_ids, validator_node_ids,
        "local founder NodeIds differ from finalized ValidatorSet bindings"
    );
    assert_eq!(
        founder_onchain_node_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        4,
        "founder Radicle NodeIds are not unique"
    );

    world.state.radicle.genesis_hash = Some(genesis_hash.clone());
    world.state.radicle.validator_node_ids = validator_node_ids.clone();
    world.state.radicle.validator_native_node_ids = validator_native_node_ids.clone();
    world.state.radicle.founder_validator_addresses = founder_validator_addresses.clone();
    world.state.radicle.founder_onchain_node_ids = founder_onchain_node_ids.clone();
    world.state.radicle.validator_sidecar_pids_before = (0..4)
        .map(|index| {
            world
                .localnet
                .radicle_pid(index)
                .expect("Radicle sidecar PID")
        })
        .collect();

    let mut signed = None;
    wait_until("canonical signed full mesh", 120, || {
        signed = signed_mesh_snapshot(
            world,
            &validator_node_ids,
            &founder_validator_addresses,
            &genesis_hash,
        );
        signed.is_some()
    });
    world.state.radicle.signed_endpoint_frames = signed.expect("signed endpoint frames");

    let mut sessions = None;
    wait_until("native Heartwood full mesh", 120, || {
        sessions = native_mesh_snapshot(world, &validator_native_node_ids);
        sessions.is_some()
    });
    world.state.radicle.initial_native_session_sets = sessions.expect("native session sets");
}

#[when("an offline user repository is registered by a funded non-validator")]
fn register_offline(world: &mut World) {
    const REPOSITORY_OWNER_GAS_FUNDING_COEN: u64 = 10;

    let fixture = world
        .localnet
        .prepare_user_radicle_repository()
        .expect("prepare offline user repository");
    let funding = world
        .rpc
        .fund_key(
            &world.validators.get(0),
            USER_KEY,
            REPOSITORY_OWNER_GAS_FUNDING_COEN,
        )
        .expect("fund non-validator repository owner");
    assert!(world.rpc.wait_successful_receipt(&funding, 40));

    let repo_bytes = hex::decode(&fixture.repo_id_hex).expect("repository OID hex");
    let repo_id: [u8; 20] = repo_bytes.try_into().expect("repository OID is 20 bytes");
    let transaction = world
        .rpc
        .register_radicle_repository(USER_KEY, repo_id)
        .expect("register public repository");
    let receipt_height = world
        .rpc
        .receipt_block_number(&transaction, world.validators.primary_port())
        .expect("repository registration block");
    for index in 0..4 {
        assert!(world.rpc.wait_finalized_at_least(
            world.validators.http_port(index),
            receipt_height,
            80,
        ));
    }

    world.state.radicle.repo_id = Some(fixture.repo_id);
    world.state.radicle.repo_id_hex = Some(fixture.repo_id_hex);
    world.state.radicle.issue_id = Some(fixture.issue_id);
    world.state.radicle.patch_id = Some(fixture.patch_id);
    world.state.radicle.source_home = Some(fixture.home);
    world.state.radicle.source_repository = Some(fixture.worktree);
    world.state.radicle.registration_transaction = Some(transaction);
    world.state.radicle.registration_finalized_height = Some(receipt_height);
}

#[then("every validator reports the finalized repository as pending while consensus advances")]
fn pending(world: &mut World) {
    let registry_repo = world.state.radicle.repo_id_hex.clone().expect("repo OID");
    wait_repositories(world, &registry_repo, "pending");
    let native_repo = world
        .state
        .radicle
        .repo_id
        .clone()
        .expect("canonical repo ID");
    let mut seeded = Vec::new();
    wait_until("native Seed Scope::All", 60, || {
        seeded = seed_scope_all(world, &native_repo, 4);
        seeded.len() == 4
    });
    assert_eq!(seeded, vec![0, 1, 2, 3]);
    world.state.radicle.initial_seed_scope_all_validators = seeded;
    let port = world.validators.primary_port();
    let before = world
        .rpc
        .finalized(port)
        .expect("finalized height before pending wait");
    let after = world
        .rpc
        .wait_block_gt(port, before, 30)
        .expect("consensus progress while repository is pending");
    assert!(
        after > before,
        "consensus did not advance while repository was pending"
    );
}

#[when("the independent source comes online and publishes its Git update")]
fn source_online(world: &mut World) {
    let mut fixture = fixture(world);
    world
        .localnet
        .start_user_radicle(&fixture)
        .expect("start independent source");
    world
        .localnet
        .push_user_radicle_update(&mut fixture)
        .expect("publish ordinary Git update");
    world.state.radicle.pushed_commit = fixture.pushed_commit;
}

#[then("every validator reports the repository available with its issue patch and Git head")]
fn available(world: &mut World) {
    let repo = world.state.radicle.repo_id_hex.clone().expect("repo ID");
    wait_repositories(world, &repo, "available");
    let fixture = fixture(world);
    wait_until("issue, patch and Git head on every validator", 180, || {
        (0..4).all(|index| {
            world
                .localnet
                .radicle_repo_visible(index, &fixture)
                .expect("observe repository through validator Radicle process")
        })
    });
}

#[when("validator 0 changes only its Radicle peer port")]
fn change_endpoint(world: &mut World) {
    let node_pid = world.localnet.validator_pid(0).expect("validator-0 PID");
    let old_sidecar = world.localnet.radicle_pid(0).expect("radicle-0 PID");
    let old_port = world
        .localnet
        .validator_radicle_addresses(0)
        .expect("old advertised endpoint")
        .into_iter()
        .find_map(|address| endpoint_port(&address))
        .expect("old Radicle port");
    let native_node_id = world.state.radicle.validator_native_node_ids[0].clone();
    world.state.radicle.endpoint_old_session_addresses = (1..4)
        .map(|index| {
            world
                .localnet
                .radicle_connected_session_address(index, &native_node_id)
                .expect("old native session to validator-0")
        })
        .collect();
    let port = world
        .localnet
        .restart_radicle_on_alternate_port(0)
        .expect("restart sidecar on alternate port");
    let node_pid_after = world.localnet.validator_pid(0).unwrap();
    let sidecar_pid_after = world.localnet.radicle_pid(0).unwrap();
    assert_eq!(node_pid_after, node_pid);
    assert_ne!(sidecar_pid_after, old_sidecar);
    world.state.radicle.endpoint_node_pid_before = Some(node_pid);
    world.state.radicle.endpoint_node_pid_after = Some(node_pid_after);
    world.state.radicle.endpoint_sidecar_pid_before = Some(old_sidecar);
    world.state.radicle.endpoint_sidecar_pid_after = Some(sidecar_pid_after);
    world.state.radicle.endpoint_old_port = Some(old_port);
    world.state.radicle.endpoint_new_port = Some(port);
}

#[then("the signed endpoint converges without restarting validator 0")]
fn endpoint_converges(world: &mut World) {
    let expected_port = world
        .state
        .radicle
        .endpoint_new_port
        .expect("alternate port");
    let old_port = world
        .state
        .radicle
        .endpoint_old_port
        .expect("original port");
    let node_id = world.state.radicle.validator_node_ids[0].clone();
    let mut replacement_frames = None;
    wait_until("replacement endpoint evidence", 120, || {
        let frames = (1..4)
            .map(|index| {
                world
                    .rpc
                    .radicle_peers(world.validators.http_port(index))
                    .and_then(|peers| peers.as_array().cloned())
                    .and_then(|peers| {
                        peers.into_iter().find(|peer| {
                            peer["nodeId"] == node_id
                                && peer["addresses"].as_array().is_some_and(|addresses| {
                                    addresses.iter().any(|address| {
                                        address.as_str().is_some_and(|address| {
                                            address.ends_with(&format!(":{expected_port}"))
                                        })
                                    })
                                })
                        })
                    })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(frames) = frames {
            replacement_frames = Some(frames);
            true
        } else {
            false
        }
    });
    world.state.radicle.endpoint_replacement_signed_frames =
        replacement_frames.expect("replacement signed endpoint frames");
    let native_node_id = world.state.radicle.validator_native_node_ids[0].clone();
    let old_addresses = world.state.radicle.endpoint_old_session_addresses.clone();
    let mut new_addresses = None;
    wait_until("native replacement sessions", 120, || {
        let observed = (1..4)
            .map(|index| {
                world
                    .localnet
                    .radicle_connected_session_address(index, &native_node_id)
            })
            .collect::<Result<Vec<_>, _>>();
        let Ok(observed) = observed else {
            return false;
        };
        if observed
            .iter()
            .zip(&old_addresses)
            .any(|(new, old)| new == old || new.ends_with(&format!(":{old_port}")))
        {
            return false;
        }
        new_addresses = Some(observed);
        true
    });
    world.state.radicle.endpoint_new_session_addresses =
        new_addresses.expect("new native session addresses");
    let mut replacement_sessions = None;
    wait_until("replacement native full mesh", 120, || {
        replacement_sessions =
            native_mesh_snapshot(world, &world.state.radicle.validator_native_node_ids);
        replacement_sessions.is_some()
    });
    world.state.radicle.endpoint_replacement_session_sets =
        replacement_sessions.expect("replacement native session sets");
}

#[when("validator 1 Radicle sidecar is stopped")]
fn stop_sidecar(world: &mut World) {
    world.state.radicle.finality_before_sidecar_fault =
        world.rpc.finalized(world.validators.primary_port());
    world.state.radicle.sidecar_fault_pid_before =
        Some(world.localnet.radicle_pid(1).expect("radicle-1 PID"));
    world.localnet.stop_radicle(1).expect("stop radicle-1");
}

#[then("consensus advances while validator 1 reports Radicle degraded")]
fn degraded(world: &mut World) {
    let before = world
        .state
        .radicle
        .finality_before_sidecar_fault
        .expect("pre-fault finality");
    let after = world
        .rpc
        .wait_block_gt(world.validators.primary_port(), before, 40)
        .unwrap_or(before);
    assert!(after > before, "sidecar failure stopped consensus");
    world.state.radicle.finality_after_sidecar_fault = Some(after);
    wait_until("validator-1 RuntimeDegraded", 60, || {
        world
            .rpc
            .radicle_status(world.validators.http_port(1))
            .is_some_and(|status| status["phase"] == "runtimeDegraded")
    });
}

#[when("validator 1 Radicle sidecar is restarted")]
fn restart_sidecar(world: &mut World) {
    world
        .localnet
        .restart_radicle(1)
        .expect("restart radicle-1");
    let recovered = world
        .localnet
        .radicle_pid(1)
        .expect("restarted radicle-1 PID");
    assert_ne!(
        Some(recovered),
        world.state.radicle.sidecar_fault_pid_before,
        "sidecar restart reused the terminated process"
    );
    world.state.radicle.sidecar_recovery_pid_after = Some(recovered);
}

#[then("validator 1 returns to Radicle ready with the same identity")]
fn sidecar_recovers(world: &mut World) {
    let node_id = world.state.radicle.validator_node_ids[1].clone();
    wait_until("validator-1 Radicle recovery", 120, || {
        world
            .rpc
            .radicle_status(world.validators.http_port(1))
            .is_some_and(|status| status["phase"] == "ready" && status["localNodeId"] == node_id)
    });
    assert_recovered_data_plane(world, 1, "sidecar recovery");
    world.state.radicle.sidecar_recovery_session_sets =
        native_mesh_snapshot(world, &world.state.radicle.validator_native_node_ids)
            .expect("sidecar recovery native sessions");
    world.state.radicle.sidecar_recovery_seed_scope_all = Some(true);
}

#[when("validator 2 node restarts while its Radicle sidecar stays alive")]
fn restart_node(world: &mut World) {
    let sidecar = world.localnet.radicle_pid(2).expect("radicle-2 PID");
    let node = world.localnet.validator_pid(2).expect("validator-2 PID");
    world.state.radicle.node_restart_sidecar_pid_before = Some(sidecar);
    world.state.radicle.node_restart_pid_before = Some(node);
    world.state.radicle.finality_before_node_restart =
        world.rpc.finalized(world.validators.primary_port());
    world
        .localnet
        .restart_validator_preserving_enclave(2)
        .expect("restart validator-2 only");
}

#[then("validator 2 returns to Radicle ready without restarting its sidecar")]
fn node_recovers(world: &mut World) {
    let sidecar_after = world.localnet.radicle_pid(2).unwrap();
    let node_after = world.localnet.validator_pid(2).unwrap();
    assert_eq!(
        Some(sidecar_after),
        world.state.radicle.node_restart_sidecar_pid_before
    );
    assert_ne!(
        Some(node_after),
        world.state.radicle.node_restart_pid_before
    );
    world.state.radicle.node_restart_sidecar_pid_after = Some(sidecar_after);
    world.state.radicle.node_restart_pid_after = Some(node_after);
    wait_until("validator-2 Radicle recovery", 120, || {
        world
            .rpc
            .radicle_status(world.validators.http_port(2))
            .is_some_and(|status| status["phase"] == "ready")
    });
    assert_recovered_data_plane(world, 2, "node recovery");
    world.state.radicle.finality_after_node_restart =
        world.rpc.finalized(world.validators.primary_port());
    world.state.radicle.node_recovery_session_sets =
        native_mesh_snapshot(world, &world.state.radicle.validator_native_node_ids)
            .expect("node recovery native sessions");
    world.state.radicle.node_recovery_seed_scope_all = Some(true);
}

#[when("a full node is promoted into the validator set")]
fn promote(world: &mut World) {
    let index = world.validators.joiner_index();
    world
        .localnet
        .launch_joiner_full_node(index, 0, &[])
        .expect("launch role-neutral full node");
    wait_until("full-node Radicle disabled status", 60, || {
        world
            .rpc
            .radicle_status(world.validators.http_port(index))
            .and_then(|status| status["phase"].as_str().map(str::to_owned))
            .as_deref()
            == Some("disabled")
    });
    let primary_finalized = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("sample primary finalized height before FullNode promotion");
    wait_until("full-node finalized catch-up", 120, || {
        world
            .rpc
            .finalized(world.validators.http_port(index))
            .is_some_and(|height| height >= primary_finalized)
    });
    world.localnet.stop_joiner_full_node(index);
    world
        .localnet
        .provision_existing_node_as_joiner(index)
        .expect("register full node as validator");
    world
        .localnet
        .launch_joiner(index, &[])
        .expect("launch joining validator");
}

#[then("it publishes no endpoint before activation and joins the signed mesh after activation")]
fn joiner_endpoint(world: &mut World) {
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    wait_until("joiner local NodeId", 60, || {
        world
            .rpc
            .radicle_status(port)
            .is_some_and(|status| status["localNodeId"].is_string())
    });
    let status = world
        .rpc
        .radicle_status(port)
        .expect("joiner Radicle status");
    let node_id = status["localNodeId"]
        .as_str()
        .expect("joiner NodeId")
        .to_owned();
    world.state.radicle.joiner_node_id = Some(node_id.clone());
    assert_ne!(
        status["phase"], "ready",
        "PENDING joiner published before activation"
    );
    assert!(!committee_has_peer(world, &node_id));

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let address = world.rpc.address_of(&key).expect("joiner address");
    world.rpc.stake(&key, 1_000).expect("stake joiner");
    let confirm_ready_tx = world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let primary = world.validators.primary_port();
    let confirm_ready_height = world
        .rpc
        .receipt_block_number(&confirm_ready_tx, primary)
        .expect("confirm-ready receipt block");
    let mut planned_activation = None;
    wait_until("eligible joiner activation boundary", 240, || {
        let Some(candidate) = world
            .rpc
            .consensus_status_field(primary, "nextPlannedActivationHeight")
            .and_then(|height| height.parse::<u64>().ok())
        else {
            return false;
        };
        if !activation_includes_confirmation(
            confirm_ready_height,
            candidate,
            RADICLE_DKG_PREPARE_BLOCKS,
        ) {
            return false;
        }
        planned_activation = Some(candidate);
        true
    });
    let planned_activation = planned_activation.expect("eligible activation boundary");
    wait_until("joiner planned activation boundary", 240, || {
        world
            .rpc
            .finalized(primary)
            .is_some_and(|height| height >= planned_activation)
    });
    let activation_deadline = planned_activation + RADICLE_DKG_ACTIVATION_GRACE_BLOCKS;
    wait_until("joiner activation within configured grace", 60, || {
        if world
            .rpc
            .is_participant(primary, &address)
            .expect("observe consensus participation")
        {
            return true;
        }
        let finalized = world
            .rpc
            .finalized_result(primary)
            .expect("observe finalized height while waiting for joiner activation");
        assert!(
            finalized <= activation_deadline,
            "joiner was not active by configured DKG grace deadline {activation_deadline}"
        );
        false
    });
    wait_until("joiner signed endpoint", 120, || {
        world
            .rpc
            .radicle_status(port)
            .is_some_and(|status| status["phase"] == "ready")
            && committee_has_peer(world, &node_id)
    });
    let mut native_node_ids = world.state.radicle.validator_native_node_ids.clone();
    let joiner_native_id = world
        .localnet
        .validator_radicle_addresses(index)
        .expect("joiner native Radicle address")[0]
        .split_once('@')
        .map(|(node_id, _)| node_id.to_owned())
        .expect("joiner advertised NodeId");
    native_node_ids.push(joiner_native_id);
    let repo = world.state.radicle.repo_id_hex.clone().expect("repo ID");
    wait_repositories_count(world, &repo, "available", 5);
    wait_until("five-node native Heartwood full mesh", 180, || {
        native_mesh_snapshot(world, &native_node_ids).is_some()
    });
    assert!(
        world
            .localnet
            .radicle_seed_scope_all(index, &world.state.radicle.repo_id.clone().expect("repo"))
            .expect("observe activated joiner Seed Scope::All"),
        "activated joiner did not restore Seed Scope::All"
    );
    wait_until("repository data on activated joiner", 180, || {
        world
            .localnet
            .radicle_repo_visible(index, &fixture(world))
            .expect("observe repository data on activated joiner")
    });
    world.state.radicle.joiner_activation_finalized_height = world.rpc.finalized(port);
    world.state.radicle.final_native_session_sets =
        native_mesh_snapshot(world, &native_node_ids).expect("five-node native session sets");
    world.state.radicle.final_seed_scope_all_validators = seed_scope_all(
        world,
        &world.state.radicle.repo_id.clone().expect("repo"),
        5,
    );
    assert_eq!(
        world.state.radicle.final_seed_scope_all_validators,
        vec![0, 1, 2, 3, 4]
    );
}

#[then("no validator has a public Radicle seed session")]
fn no_public_seed(world: &mut World) {
    for index in 0..=world.validators.joiner_index() {
        let status = world
            .localnet
            .radicle_node_status(index)
            .expect("native Radicle status");
        assert!(!status.contains("iris.radicle.network"));
        assert!(!status.contains("rosa.radicle.network"));
    }
    assert_durable_evidence_complete(world);
}

fn fixture(world: &World) -> RadicleRepositoryFixtureV1 {
    RadicleRepositoryFixtureV1 {
        home: world
            .state
            .radicle
            .source_home
            .clone()
            .expect("source home"),
        worktree: world
            .state
            .radicle
            .source_repository
            .clone()
            .expect("source worktree"),
        repo_id: world.state.radicle.repo_id.clone().expect("repo ID"),
        repo_id_hex: world.state.radicle.repo_id_hex.clone().expect("repo OID"),
        issue_id: world.state.radicle.issue_id.clone().expect("issue ID"),
        patch_id: world.state.radicle.patch_id.clone().expect("patch ID"),
        pushed_commit: world.state.radicle.pushed_commit.clone(),
    }
}

fn wait_repositories(world: &World, repo: &str, expected: &str) {
    wait_repositories_count(world, repo, expected, 4);
}

fn wait_repositories_count(world: &World, repo: &str, expected: &str, count: usize) {
    wait_until("repository state convergence", 180, || {
        (0..count).all(|index| {
            world
                .rpc
                .radicle_repositories(world.validators.http_port(index))
                .and_then(|repositories| repositories.as_array().cloned())
                .is_some_and(|repositories| {
                    repositories.iter().any(|repository| {
                        repository["repoId"] == repo && repository["state"] == expected
                    })
                })
        })
    });
}

fn seed_scope_all(world: &World, repo: &str, count: usize) -> Vec<usize> {
    (0..count)
        .filter(|&index| {
            world
                .localnet
                .radicle_seed_scope_all(index, repo)
                .expect("observe validator Seed Scope::All")
        })
        .collect()
}

fn signed_mesh_snapshot(
    world: &World,
    node_ids: &[String],
    validators: &[String],
    genesis_hash: &str,
) -> Option<Vec<Vec<serde_json::Value>>> {
    let expected = node_ids
        .iter()
        .cloned()
        .zip(validators.iter().map(|value| value.to_ascii_lowercase()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut snapshots = Vec::with_capacity(node_ids.len());
    for index in 0..node_ids.len() {
        let peers = world
            .rpc
            .radicle_peers(world.validators.http_port(index))?
            .as_array()?
            .clone();
        if peers.len() != node_ids.len().saturating_sub(1) {
            return None;
        }
        let observed = peers
            .iter()
            .filter_map(|peer| peer["nodeId"].as_str())
            .collect::<BTreeSet<_>>();
        let expected_nodes = node_ids
            .iter()
            .enumerate()
            .filter(|(peer, _)| *peer != index)
            .map(|(_, node_id)| node_id.as_str())
            .collect::<BTreeSet<_>>();
        if observed != expected_nodes
            || peers.iter().any(|peer| {
                let Some(node_id) = peer["nodeId"].as_str() else {
                    return true;
                };
                let Some(validator) = peer["validator"].as_str() else {
                    return true;
                };
                expected.get(node_id).map(String::as_str) != Some(&validator.to_ascii_lowercase())
                    || peer["genesisHash"] != genesis_hash
                    || peer["senderBlsPublicKey"].as_str().map(str::len) != Some(98)
                    || peer["signature"].as_str().map(str::len) != Some(194)
                    || peer["encodedFrame"]
                        .as_str()
                        .is_none_or(|frame| frame.len() <= 2)
                    || peer["addresses"].as_array().is_none_or(Vec::is_empty)
                    || peer["validUntilHeight"].as_u64() <= peer["anchorNumber"].as_u64()
            })
        {
            return None;
        }
        snapshots.push(peers);
    }
    Some(snapshots)
}

fn native_mesh_snapshot(world: &World, node_ids: &[String]) -> Option<Vec<Vec<String>>> {
    let mut snapshots = Vec::with_capacity(node_ids.len());
    for index in 0..node_ids.len() {
        let expected = node_ids
            .iter()
            .enumerate()
            .filter(|(peer, _)| *peer != index)
            .map(|(_, node_id)| node_id.clone())
            .collect::<BTreeSet<_>>();
        let connected = world
            .localnet
            .radicle_connected_session_node_ids(index)
            .ok()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let rpc_connected = world
            .rpc
            .radicle_status(world.validators.http_port(index))?["connectedPeerCount"]
            .as_u64()?;
        snapshots.push(managed_session_snapshot(
            &expected,
            &connected,
            rpc_connected,
        )?);
    }
    Some(snapshots)
}

fn managed_session_snapshot(
    expected: &BTreeSet<String>,
    connected: &BTreeSet<String>,
    rpc_connected: u64,
) -> Option<Vec<String>> {
    if !expected.is_subset(connected) || rpc_connected < expected.len() as u64 {
        return None;
    }
    Some(expected.iter().cloned().collect())
}

fn assert_recovered_data_plane(world: &World, index: usize, label: &str) {
    let repo = world.state.radicle.repo_id.clone().expect("repo");
    wait_until(label, 180, || {
        native_mesh_snapshot(world, &world.state.radicle.validator_native_node_ids).is_some()
            && world
                .localnet
                .radicle_seed_scope_all(index, &repo)
                .expect("observe restored Seed Scope::All")
            && world
                .localnet
                .radicle_repo_visible(index, &fixture(world))
                .expect("observe restored repository visibility")
    });
}

fn endpoint_port(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

fn activation_includes_confirmation(
    confirmation_height: u64,
    planned_activation: u64,
    prepare_window: u64,
) -> bool {
    planned_activation
        .checked_sub(prepare_window)
        .is_some_and(|freeze_height| confirmation_height <= freeze_height)
}

fn committee_has_peer(world: &World, node_id: &str) -> bool {
    (0..4).all(|index| {
        world
            .rpc
            .radicle_peers(world.validators.http_port(index))
            .and_then(|peers| peers.as_array().cloned())
            .is_some_and(|peers| peers.iter().any(|peer| peer["nodeId"] == node_id))
    })
}

fn assert_durable_evidence_complete(world: &World) {
    let evidence = &world.state.radicle;
    assert_eq!(evidence.endpoint_replacement_signed_frames.len(), 3);
    assert!(evidence
        .endpoint_replacement_signed_frames
        .iter()
        .all(|frame| frame["encodedFrame"]
            .as_str()
            .is_some_and(|value| value.len() > 2)));
    assert_eq!(
        evidence.endpoint_node_pid_before,
        evidence.endpoint_node_pid_after
    );
    assert_ne!(
        evidence.endpoint_sidecar_pid_before,
        evidence.endpoint_sidecar_pid_after
    );
    assert_ne!(
        evidence.sidecar_fault_pid_before,
        evidence.sidecar_recovery_pid_after
    );
    assert_ne!(
        evidence.node_restart_pid_before,
        evidence.node_restart_pid_after
    );
    assert_eq!(
        evidence.node_restart_sidecar_pid_before,
        evidence.node_restart_sidecar_pid_after
    );
    assert!([
        evidence.endpoint_node_pid_before,
        evidence.endpoint_sidecar_pid_before,
        evidence.endpoint_sidecar_pid_after,
        evidence.sidecar_fault_pid_before,
        evidence.sidecar_recovery_pid_after,
        evidence.node_restart_pid_before,
        evidence.node_restart_pid_after,
        evidence.node_restart_sidecar_pid_before,
    ]
    .iter()
    .all(Option::is_some));
    assert!([
        evidence.finality_before_sidecar_fault,
        evidence.finality_after_sidecar_fault,
        evidence.finality_before_node_restart,
        evidence.finality_after_node_restart,
    ]
    .iter()
    .all(Option::is_some));
}

fn wait_until(label: &str, attempts: u32, mut predicate: impl FnMut() -> bool) {
    for _ in 0..attempts {
        if predicate() {
            return;
        }
        sleep(Duration::from_secs(1));
    }
    assert!(predicate(), "timed out waiting for {label}");
}

#[cfg(test)]
mod tests {
    use super::{activation_includes_confirmation, managed_session_snapshot, BTreeSet};

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn managed_mesh_allows_external_inbound_sessions() {
        let expected = set(&["validator-1", "validator-2", "validator-3"]);
        let connected = set(&[
            "validator-1",
            "validator-2",
            "validator-3",
            "developer-node",
        ]);

        assert_eq!(
            managed_session_snapshot(&expected, &connected, 3),
            Some(expected.into_iter().collect())
        );
    }

    #[test]
    fn managed_mesh_still_requires_every_validator_and_rpc_convergence() {
        let expected = set(&["validator-1", "validator-2", "validator-3"]);
        assert_eq!(
            managed_session_snapshot(
                &expected,
                &set(&["validator-1", "validator-2", "developer-node"]),
                3,
            ),
            None
        );
        assert_eq!(managed_session_snapshot(&expected, &expected, 2), None);
    }

    #[test]
    fn activation_accepts_confirmation_before_freeze() {
        assert!(activation_includes_confirmation(44, 60, 15));
    }

    #[test]
    fn activation_accepts_confirmation_at_freeze() {
        assert!(activation_includes_confirmation(45, 60, 15));
    }

    #[test]
    fn activation_rejects_confirmation_after_freeze() {
        assert!(!activation_includes_confirmation(46, 60, 15));
    }
}
