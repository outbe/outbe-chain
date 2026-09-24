//! Hardware rollout through governance and the production upgrade CLI.
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::internal::{addresses, eth};
use crate::world::{localnet::HardwareEnclaveCandidate, World};
use alloy_primitives::{keccak256, Address, B256, U256};
use cucumber::when;

fn permanent_key(world: &World, port: u16) -> U256 {
    let height = world.rpc.finalized(port).expect("finalized key checkpoint");
    eth::read_call_at_result(
        &world.rpc.url(port),
        addresses::TEE_ADDR,
        &eth::ITeeRegistryV1::tributeOfferPublicKeyCall {},
        height,
    )
    .expect("finalized permanent offer key")
}

fn binding(world: &World, port: u16, owner: Address) -> B256 {
    let height = world
        .rpc
        .finalized(port)
        .expect("finalized binding checkpoint");
    eth::read_call_at_result(
        &world.rpc.url(port),
        addresses::TEE_ADDR,
        &eth::ITeeRegistryV1::validatorEnclaveBindingCall { validator: owner },
        height,
    )
    .expect("finalized validator binding")
    .bindingId
}

#[when(expr = "the operators install node release version {string} before enclave governance")]
fn install_node_release(world: &mut World, version: String) {
    let ports = world.validators.committee_ports();
    let key = permanent_key(world, ports[0]);
    let height = world.rpc.finalized(ports[0]).expect("pre-update finality");
    world
        .localnet
        .restart_committee_with_upgraded_binary(&version)
        .expect("install separately built release node and retain enclave state");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height + 3, 180)
        .expect("replacement nodes resume finality");
    for port in ports {
        assert_eq!(
            permanent_key(world, port),
            key,
            "node update replaced network key"
        );
    }
}

#[when(
    expr = "the four validators upgrade their enclaves to version {string} using {string} in round {int}"
)]
fn upgrade_hardware_committee(world: &mut World, version: String, binary: String, round: u32) {
    assert_eq!(
        world.validators.size(),
        4,
        "hardware rollout requires four validators"
    );
    let binary = crate::env::environment().repo.join(Path::new(&binary));
    let ports = world.validators.committee_ports();
    let permanent = permanent_key(world, ports[0]);
    assert!(
        !permanent.is_zero(),
        "existing DKG must already own the permanent key"
    );
    // Only one replacement enclave is live at a time, bounding SGX TCS/EPC use.
    let mut first = Some(
        world
            .localnet
            .start_hardware_upgrade_candidate(0, round, &binary)
            .expect("start first real SGX candidate"),
    );
    let measurement = first.as_ref().unwrap().measurement.mrenclave.clone();
    let head = world.rpc.head(ports[0]).expect("upgrade proposal head");
    let activation = head + world.state.voting_window + 360;
    let payload = serde_json::json!({"version": version, "activationHeight": activation,
        "info": "hardware enclave replacement", "mrenclave": format!("0x{measurement}")})
    .to_string();
    let proposer = world
        .validators
        .operator("validator-0")
        .expect("upgrade proposer");
    let tx = world
        .rpc
        .send_propose(
            &proposer,
            &format!("{:#x}", addresses::UPDATE_ADDR),
            &payload,
        )
        .expect("propose measured enclave successor");
    assert!(world.rpc.wait_successful_receipt(&tx, 60));
    let proposal = world
        .rpc
        .proposal_id_from_receipt(ports[0], &tx)
        .expect("exact proposal receipt");
    for index in 0..4 {
        let tx = world
            .rpc
            .cast_vote(&world.validators.get(index), proposal, true)
            .expect("vote for successor");
        assert!(world.rpc.wait_successful_receipt(&tx, 60));
        if world
            .rpc
            .vote_status(proposal)
            .expect("proposal status")
            .status
            == "approved"
        {
            break;
        }
    }
    let status = world.rpc.vote_status(proposal).expect("proposal deadline");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, status.deadline.expect("voting deadline") + 1, 120)
        .expect("successor policy finalized");
    assert_eq!(
        world
            .rpc
            .vote_status(proposal)
            .expect("finalized proposal")
            .status,
        "approved"
    );
    let genesis = world.localnet.scenario_dir().join("genesis.json");
    let mut signers = std::collections::BTreeSet::new();
    for index in 0..4 {
        let donor = if index == 0 { 3 } else { 0 };
        let port = ports[donor];
        let mut candidate: HardwareEnclaveCandidate = if index == 0 {
            first.take().unwrap()
        } else {
            world
                .localnet
                .start_hardware_upgrade_candidate(index, round, &binary)
                .expect("start successor")
        };
        assert_eq!(
            candidate.measurement.mrenclave, measurement,
            "operator signing changed code identity"
        );
        assert!(
            signers.insert(candidate.measurement.mrsigner.clone()),
            "operators must sign independently"
        );
        let owner = world
            .rpc
            .address_of(&world.validators.get(index).evm_key().unwrap())
            .unwrap()
            .parse::<Address>()
            .unwrap();
        let before = binding(world, port, owner);
        let active_dir = world
            .localnet
            .active_enclave_seal_directory(index)
            .expect("active enclave path");
        world
            .localnet
            .run_candidate_upgrade_cli(
                &candidate,
                donor,
                "upgrade-prepare",
                &[
                    "--active-tee-dir".into(),
                    active_dir.display().to_string(),
                    "--candidate-tee-dir".into(),
                    candidate.tee_dir().display().to_string(),
                ],
            )
            .expect("prepare authorized candidate");
        let id = keccak256(format!("hardware-upgrade/{}/{index}", candidate.round));
        let timestamp = world
            .rpc
            .block_timestamp(port, world.rpc.finalized(port).unwrap())
            .unwrap();
        let provision = vec![
            "--genesis".into(),
            genesis.display().to_string(),
            "--binding-id".into(),
            format!("{id:#x}"),
            "--valid-until".into(),
            (timestamp + 7200).to_string(),
        ];
        world
            .localnet
            .run_candidate_upgrade_cli(&candidate, donor, "upgrade-provision", &provision)
            .expect("transfer existing network key through finalized authorization");
        assert_eq!(
            binding(world, port, owner),
            before,
            "preparation replaced active binding"
        );
        let blob = std::fs::read(candidate.tee_dir().join("sealed_root.bin"))
            .expect("private candidate seal is readable by its operator");
        assert!(
            blob.starts_with(b"TSGX1"),
            "candidate did not persist a combined hardware seal"
        );
        world
            .localnet
            .restart_hardware_upgrade_candidate(&mut candidate)
            .expect("restart sealed candidate");
        world
            .localnet
            .run_candidate_upgrade_cli(&candidate, donor, "upgrade-provision", &provision)
            .expect("retry after candidate restart retains exact authorization and key");
        if index == 0 && round == 1 {
            let finalized = world
                .rpc
                .finalized(port)
                .expect("prepared candidate finality");
            let active = eth::read_call_at_result(
                &world.rpc.url(port),
                addresses::TEE_ADDR,
                &eth::ITeeRegistryV1::validatorEnclaveBindingCall { validator: owner },
                finalized,
            )
            .expect("active node identity");
            let pending_call = eth::ITeeRegistryV1::pendingEnclaveUpgradeCall {
                nodeIdHash: active.nodeIdHash,
            };
            let pending = eth::read_call_at_result(
                &world.rpc.url(port),
                addresses::TEE_ADDR,
                &pending_call,
                finalized,
            )
            .expect("finalized prepared candidate");
            assert!(!pending.contextHash.is_zero());
            let key = world
                .validators
                .get(index)
                .evm_key()
                .expect("candidate operator key");
            let tx = eth::send_call(
                &world.rpc.url(port),
                addresses::TEE_ADDR,
                &key,
                &eth::ITeeRegistryV1::cancelEnclaveUpgradeCall {
                    nodeIdHash: active.nodeIdHash,
                    expectedContextHash: pending.contextHash,
                },
                None,
            )
            .expect("operator cancels exact candidate");
            assert!(world.rpc.wait_successful_receipt(&tx, 60));
            let height = world.rpc.head(port).expect("cancellation head");
            world
                .rpc
                .wait_finalized_checkpoint(&ports, height, 120)
                .expect("cancellation finality");
            let cancelled = eth::read_call_at_result(
                &world.rpc.url(port),
                addresses::TEE_ADDR,
                &pending_call,
                height,
            )
            .expect("cancelled candidate state");
            assert!(cancelled.contextHash.is_zero());
            assert_eq!(
                cancelled.nonce, pending.nonce,
                "cancellation must retain replay protection"
            );
            assert_eq!(
                binding(world, port, owner),
                before,
                "cancellation replaced active binding"
            );
            let mut retry = provision.clone();
            retry.push("--new-attempt".into());
            world
                .localnet
                .run_candidate_upgrade_cli(&candidate, donor, "upgrade-provision", &retry)
                .expect("fresh nonce after explicit cancellation");
            let height = world.rpc.finalized(port).unwrap();
            let renewed = eth::read_call_at_result(
                &world.rpc.url(port),
                addresses::TEE_ADDR,
                &pending_call,
                height,
            )
            .expect("fresh prepared authority");
            assert_eq!(renewed.nonce, pending.nonce + 1);
            assert_ne!(renewed.contextHash, pending.contextHash);
        }
        world
            .localnet
            .run_candidate_upgrade_cli(
                &candidate,
                donor,
                "upgrade-submit",
                &[
                    "--binding-id".into(),
                    format!("{id:#x}"),
                    "--valid-until".into(),
                    (timestamp + 7200).to_string(),
                ],
            )
            .expect("submit resident-key transition");
        let deadline = Instant::now() + Duration::from_secs(120);
        while binding(world, port, owner) != id {
            assert!(
                Instant::now() < deadline,
                "candidate transition was not finalized"
            );
            sleep(Duration::from_millis(500));
        }
        if world.localnet.validator_running(index) {
            world
                .localnet
                .kill_validator(index)
                .expect("stop exact predecessor node");
        }
        world
            .localnet
            .run_candidate_upgrade_cli(&candidate, donor, "upgrade-finalize", &[])
            .expect("persist catch-up anchor and promote B with node stopped");
        world
            .localnet
            .select_promoted_hardware_candidate(candidate, donor)
            .expect("select exact promoted enclave");
        let marker = "local TEE recovery anchor durably persisted; validator restart ready";
        let previous = world
            .localnet
            .log_count(index, marker)
            .expect("capture recovery marker baseline");
        let follower = world
            .localnet
            .launch_validator_recovery_follower(index, donor)
            .expect("start certified recovery follower");
        let deadline = Instant::now() + Duration::from_secs(180);
        while world
            .localnet
            .log_count(index, marker)
            .expect("read recovery progress")
            <= previous
        {
            assert!(
                Instant::now() < deadline,
                "recovery anchor was not durably committed"
            );
            sleep(Duration::from_millis(500));
        }
        world
            .localnet
            .stop_follower(&follower)
            .expect("stop recovered follower");
        world
            .localnet
            .restart_validator(index)
            .expect("restore validator authority");
        let height = world
            .rpc
            .finalized(port)
            .expect("donor finality after restart");
        world
            .rpc
            .wait_finalized_checkpoint(&ports, height + 3, 120)
            .expect("all four validators resumed finality");
        for &peer in &ports {
            assert_eq!(
                permanent_key(world, peer),
                permanent,
                "permanent network key changed"
            );
        }
    }
    world
        .rpc
        .wait_finalized_checkpoint(&ports, activation + 3, 900)
        .expect("all four nodes passed retirement height");
    for index in 0..4 {
        let owner = world
            .rpc
            .address_of(&world.validators.get(index).evm_key().unwrap())
            .unwrap()
            .parse::<Address>()
            .unwrap();
        assert!(eth::read_call_result(
            &world.rpc.url(ports[0]),
            addresses::TEE_ADDR,
            &eth::ITeeRegistryV1::isValidatorEnclaveReadyCall { validator: owner }
        )
        .expect("post-retirement readiness"));
    }
    eprintln!("HARDWARE_ENCLAVE_UPGRADE round={round} version={version} proposal={proposal} activation={activation} mrenclave={measurement} permanent_key={permanent:#x}");
}
