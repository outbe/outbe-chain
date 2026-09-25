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

fn binding(world: &World, port: u16, index: usize) -> B256 {
    binding_view(world, port, index).bindingId
}

fn binding_view(
    world: &World,
    port: u16,
    index: usize,
) -> outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1View {
    use alloy_signer_local::PrivateKeySigner;
    use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;

    let directory = world.validators.data_dir(index);
    let secret = std::fs::read_to_string(directory.parent().unwrap().join("reth-p2p-secret.hex"))
        .expect("existing NodeHost P2P identity");
    let signer: PrivateKeySigner = secret.trim().parse().expect("valid NodeHost P2P key");
    let encoded = signer.credential().verifying_key().to_encoded_point(true);
    let public = encoded.as_bytes();
    let height = world
        .rpc
        .finalized(port)
        .expect("finalized binding checkpoint");
    eth::read_call_at_result(
        &world.rpc.url(port),
        addresses::TEE_ADDR,
        &ITeeRegistryV1::nodeHostEnclaveBindingCall {
            rethP2pPrefix: public[0],
            rethP2pX: B256::from_slice(&public[1..]),
        },
        height,
    )
    .expect("finalized validator binding")
}

#[when(expr = "the operators install node release version {string} before enclave governance")]
fn install_node_release(world: &mut World, version: String) {
    let ports = world.validators.committee_ports();
    let key = permanent_key(world, ports[0]);
    let height = world.rpc.finalized(ports[0]).expect("pre-update finality");
    let price = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = suspend_computation(world);
    let full_node = if world.state.ocomp_successor_bundle_hash.is_some() {
        let index = world.validators.joiner_index();
        let (pid, exit) = world
            .localnet
            .owned_full_node_process(index)
            .expect("owned FullNode before node update");
        assert!(exit.is_none());
        world
            .ocomp
            .stop_keyless_full_node_roles(index.try_into().unwrap())
            .expect("stop FullNode clients");
        Some((index, pid))
    } else {
        None
    };

    world
        .localnet
        .restart_committee_with_upgraded_binary(&version)
        .expect("install separately built release node and retain enclave state");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height + 3, 180)
        .expect("replacement nodes resume finality");
    if let Some((index, pid)) = full_node {
        let (_, new_pid) = world
            .localnet
            .restart_keyless_full_node_preserving_enclave(index, pid, 0)
            .expect("update FullNode binary with its existing identity and data");
        *world
            .state
            .ocomp_successor_node_pids_after_activation
            .last_mut()
            .expect("FullNode observation") = new_pid;
        world
            .ocomp
            .start_keyless_full_node_roles(index.try_into().unwrap())
            .expect("restore FullNode clients");
        let mut all = ports.clone();
        all.push(world.validators.http_port(index));
        world
            .rpc
            .wait_finalized_checkpoint(&all, height + 3, 180)
            .expect("updated FullNode catches up");
    }
    resume_computation(world, clients, price);

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
    let price = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = suspend_computation(world);
    assert!(
        !permanent.is_zero(),
        "existing DKG must already own the permanent key"
    );
    let full_node = world.state.ocomp_successor_bundle_hash.map(|_| {
        let index = world.validators.joiner_index();
        let (pid, exit) = world
            .localnet
            .owned_full_node_process(index)
            .expect("owned FullNode before enclave rollout");
        assert!(exit.is_none(), "FullNode exited before enclave rollout");
        world
            .ocomp
            .stop_keyless_full_node_roles(index.try_into().unwrap())
            .expect("stop FullNode clients before enclave rollout");
        (index, pid)
    });
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
    // Ballots have independent senders. Submit all four within the voting
    // window before waiting for finality: the main scenario uses six blocks,
    // so four sequential send-and-finalize cycles can miss its deadline.
    let ballots: Vec<_> = (0..4)
        .map(|index| {
            world
                .rpc
                .cast_vote(&world.validators.get(index), proposal, true)
                .expect("vote for successor")
        })
        .collect();
    for tx in ballots {
        assert!(
            world.rpc.wait_successful_receipt(&tx, 60),
            "successor ballot failed: proposal={proposal} tx={tx}"
        );
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
    for index in (0..4).chain(full_node.map(|(index, _)| index)) {
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
        let before = binding(world, port, index);
        assert_ne!(
            before,
            B256::ZERO,
            "upgrade requires the existing NodeHost binding"
        );
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
        // Match normal NodeHost admission: the business scenario advances the
        // consensus clock across UTC days after each enclave replacement.
        let valid_until = timestamp
            .checked_add(outbe_primitives::tee_genesis_v1::PRODUCTION_TEE_LEASE_SECONDS_V1)
            .expect("replacement NodeHost lease deadline");
        let provision = vec![
            "--genesis".into(),
            genesis.display().to_string(),
            "--binding-id".into(),
            format!("{id:#x}"),
            "--valid-until".into(),
            valid_until.to_string(),
        ];
        world
            .localnet
            .run_candidate_upgrade_cli(&candidate, donor, "upgrade-provision", &provision)
            .expect("transfer existing network key through finalized authorization");
        assert_eq!(
            binding(world, port, index),
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
                binding(world, port, index),
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
        if let Some((full_index, pid)) = full_node.filter(|(slot, _)| *slot == index) {
            world
                .localnet
                .stop_joiner_full_node_owned(full_index, pid)
                .expect("stop exact FullNode before committing its enclave transition");
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
                    valid_until.to_string(),
                ],
            )
            .expect("submit resident-key transition");
        let deadline = Instant::now() + Duration::from_secs(120);
        while binding(world, port, index) != id {
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
        if index < 4 {
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
        } else {
            world
                .localnet
                .launch_dcap_full_node(&format!("joiner-full-node-{index}"), index, donor)
                .expect(
                    "restore non-voting FullNode using its promoted enclave and preserved datadir",
                );
            let (pid, exit) = world
                .localnet
                .owned_full_node_process(index)
                .expect("replacement FullNode owner");
            assert!(exit.is_none());
            assert_ne!(pid, full_node.unwrap().1);
            *world
                .state
                .ocomp_successor_node_pids_after_activation
                .last_mut()
                .expect("FullNode PID observation") = pid;
        }
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
    if let Some((index, _)) = full_node {
        world
            .ocomp
            .start_keyless_full_node_roles(index.try_into().unwrap())
            .expect("restore existing FullNode computation domain after enclave migration");
        let mut all_ports = ports.clone();
        all_ports.push(world.validators.http_port(index));
        world
            .rpc
            .wait_finalized_checkpoint(&all_ports, activation + 3, 180)
            .expect("FullNode retains certified finality after measurement retirement");
        assert_eq!(
            world.rpc.active_count(ports[0]),
            Some(4),
            "FullNode acquired validator authority"
        );
        assert!(
            !world
                .validators
                .data_dir(index)
                .parent()
                .unwrap()
                .join("ocomp-key-v1.hex")
                .exists(),
            "FullNode acquired an OCOMP voting key"
        );
        assert_eq!(
            world
                .rpc
                .state_root(world.validators.http_port(index), activation + 3),
            world.rpc.state_root(ports[0], activation + 3),
            "FullNode state differs after enclave migration"
        );
    }
    for index in (0..4).chain(full_node.map(|(index, _)| index)) {
        let donor = if index == 0 { 3 } else { 0 };
        let before = binding(world, ports[donor], index);
        let output = world
            .localnet
            .renew_active_hardware_enclave(index, donor)
            .expect("normal tee renew must work after promoted enclave retirement");
        assert!(
            output.contains("NotDue"),
            "fresh lease unexpectedly due: {output}"
        );
        assert_eq!(
            binding(world, ports[donor], index),
            before,
            "checking a fresh lease must not replace its binding"
        );
        assert_eq!(permanent_key(world, ports[donor]), permanent);
        eprintln!("HARDWARE_ENCLAVE_RENEWAL round={round} node={index} outcome=not_due");
    }
    resume_computation(world, clients, price);
    renew_promoted_leases_when_due(world, full_node.map(|(index, _)| index), round, permanent);
    eprintln!("HARDWARE_ENCLAVE_UPGRADE round={round} version={version} proposal={proposal} activation={activation} mrenclave={measurement} permanent_key={permanent:#x}");
}

fn renew_promoted_leases_when_due(
    world: &mut World,
    full_node: Option<usize>,
    round: u32,
    permanent: U256,
) {
    let ports = world.validators.committee_ports();
    let lease = outbe_primitives::tee_genesis_v1::PRODUCTION_TEE_LEASE_SECONDS_V1;
    let before: Vec<_> = (0..4)
        .chain(full_node)
        .map(|index| (index, binding_view(world, ports[0], index)))
        .collect();
    let target = before
        .iter()
        .map(|(_, binding)| binding.validUntil.checked_sub(lease / 2).unwrap())
        .max()
        .unwrap()
        + 1;
    let (_, before_restart, _, mut pending) =
        crate::features::ocomp::restart_committee_at_logical_time(world, target);
    // Production consensus advances at most one hour per block. A week-long
    // lease jump needs at least 168 blocks, so bound a stalled clock rather
    // than the total time needed to reach the renewal window. The scenario
    // still has its overall execution deadline.
    let stall_timeout = Duration::from_secs(180);
    let mut last_timestamp = before_restart[0].block_timestamp;
    let mut deadline = Instant::now() + stall_timeout;
    eprintln!("HARDWARE_ENCLAVE_RENEWAL_CLOCK round={round} from={last_timestamp} target={target}");
    loop {
        let height = ports
            .iter()
            .map(|&port| world.rpc.finalized(port).expect("lease window checkpoint"))
            .min()
            .expect("renewal requires the validator committee");
        let timestamp = world.rpc.block_timestamp(ports[0], height).unwrap();
        if timestamp > last_timestamp {
            last_timestamp = timestamp;
            deadline = Instant::now() + stall_timeout;
        }
        // Observe the feeder concurrently with the clock ratchet, retaining
        // success instead of starting its observation after a long jump.
        if pending.as_ref().is_some_and(|publication| {
            crate::features::price_oracle::observe_pending_publication(world, publication)
        }) {
            pending = None;
        }
        if timestamp >= target && pending.is_none() {
            eprintln!(
                "HARDWARE_ENCLAVE_RENEWAL_CLOCK round={round} reached={timestamp} height={height}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "consensus clock stalled before lease renewal window: timestamp={timestamp} target={target} height={height}"
        );
        sleep(Duration::from_millis(500));
    }
    for (index, old) in before {
        let donor = if index == 0 { 3 } else { 0 };
        let output = world
            .localnet
            .renew_active_hardware_enclave(index, donor)
            .expect("renew promoted hardware enclave lease through normal CLI");
        assert!(
            output.contains("Finalized"),
            "due renewal did not finalize: {output}"
        );
        let renewed = binding_view(world, ports[donor], index);
        assert_eq!(renewed.bindingId, old.bindingId);
        assert_eq!(renewed.enclaveId, old.enclaveId);
        assert_eq!(renewed.bindingVersion, old.bindingVersion);
        assert_eq!(renewed.transitionNonce, old.transitionNonce);
        assert_eq!(renewed.registrationVersion, old.registrationVersion + 1);
        assert_eq!(renewed.renewalNonce, old.renewalNonce + 1);
        assert_eq!(renewed.validUntil, old.validUntil + lease);
        assert_eq!(permanent_key(world, ports[donor]), permanent);
        let repeated = world
            .localnet
            .renew_active_hardware_enclave(index, donor)
            .expect("repeat normal renewal after finalization");
        assert!(
            repeated.contains("NotDue"),
            "finalized lease renewed twice: {repeated}"
        );
        assert_eq!(
            binding_view(world, ports[donor], index).renewalNonce,
            renewed.renewalNonce
        );
        eprintln!("HARDWARE_ENCLAVE_RENEWAL round={round} node={index} outcome=finalized old_until={} new_until={} nonce={}", old.validUntil, renewed.validUntil, renewed.renewalNonce);
    }
}

fn suspend_computation(
    world: &mut World,
) -> Option<crate::world::ocomp::OcompNodeFacingResumePlan> {
    world.state.ocomp_successor_bundle_hash.map(|_| {
        world
            .ocomp
            .suspend_node_facing_roles()
            .expect("suspend exact computation clients during hardware rollout")
    })
}

fn resume_computation(
    world: &mut World,
    clients: Option<crate::world::ocomp::OcompNodeFacingResumePlan>,
    price: Option<u64>,
) {
    if let Some(clients) = clients {
        world
            .ocomp
            .resume_node_facing_roles(clients)
            .expect("restore exact computation clients after hardware rollout");
    }
    if let Some(pending) = crate::features::price_oracle::resume_after_clock_restart(world, price) {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            assert!(
                Instant::now() < deadline,
                "feeder did not resume after hardware rollout"
            );
            sleep(Duration::from_millis(500));
        }
    }
}
