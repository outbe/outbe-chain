//! Restart steps used by `features/validator_lifecycle.feature`. An ACTIVE validator's DKG share lives on
//! disk (keys-dir), not the enclave. Killing and restarting ONLY the node (the
//! enclave container stays up) must resume signing from the persisted share
//! WITHOUT a fresh DKG ceremony.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use serde_json::json;

use crate::features::common::boot_localnet;
use crate::internal::{addresses, eth, launch_log::LaunchLog, pending_dkg::PendingDkgCheckpoint};
use crate::world::rpc::{FinalizedCheckpoint, Rpc, TxOutcome};
use crate::world::state::{PendingDkgRestartState, RestartIncarnation};
use crate::world::World;

/// Put the freeze boundary inside the bounded restart scenario while leaving
/// enough activation grace for a real-SGX ceremony to recover.
#[given("a fresh localnet with a restartable DKG window")]
fn restartable_dkg_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "120".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "120".to_string()),
        ],
    );
}

// Observation helpers for cases 2/5/6/7 only. Cases 3/4 retain their own proofs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RestartPublicState {
    address: alloy_primitives::Address,
    consensus_public_key: alloy_primitives::Bytes,
    p2p_version: u8,
    p2p_encoded: alloy_primitives::Bytes,
    epoch: u64,
    status: u8,
    stake: alloy_primitives::U256,
    active: Vec<alloy_primitives::Address>,
    participants: Vec<alloy_primitives::Address>,
    voter_misses: u64,
    supply: alloy_primitives::U256,
}

fn restart_ports(world: &World) -> Result<Vec<u16>> {
    ensure!(
        world.validators.size() == 4 && world.validators.joiner_index() == 4,
        "restart proof requires four founders and the fifth node"
    );
    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(4));
    ensure!(
        ports.len() == 5
            && ports
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 5,
        "restart proof requires five distinct expected RPCs"
    );
    Ok(ports)
}

fn restart_assert_live(world: &mut World) -> Result<()> {
    ensure!(
        world.state.lifecycle_incarnations.len() == 5,
        "missing restart process owner"
    );
    for index in 0..5 {
        let expected = world
            .state
            .lifecycle_incarnations
            .get(&index)
            .ok_or_else(|| eyre!("missing restart owner {index}"))?;
        let expected = (expected.node_pid, expected.enclave_pid);
        ensure!(
            world.localnet.live_validator_and_enclave_pids(index)? == expected,
            "restart proof process incarnation changed for validator-{index}"
        );
    }
    Ok(())
}

fn restart_capture_incarnations(world: &mut World, restarted: usize) -> Result<()> {
    restart_ports(world)?;
    ensure!(
        world.state.lifecycle_incarnations.is_empty(),
        "restart proof already armed"
    );
    for index in 0..5 {
        let (node_pid, enclave_pid) = world.localnet.live_validator_and_enclave_pids(index)?;
        let dir = world
            .localnet
            .scenario_dir()
            .join(format!("validator-{index}"));
        let node_log = LaunchLog::checkpoint(&dir.join("node.log"))?;
        let enclave_log = LaunchLog::checkpoint(&dir.join("enclave.log"))?;
        world.state.restart_observations.push(json!({
            "phase": "restart_owner_armed", "index": index,
            "node_pid": node_pid, "enclave_pid": enclave_pid,
            "node_log_start": node_log.start_offset(), "enclave_log_start": enclave_log.start_offset(),
            "datadir": dir.join("data"),
        }));
        world.state.lifecycle_incarnations.insert(
            index,
            RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            },
        );
    }
    let offer_public = world.localnet.node_offer_public(restarted)?;
    world.state.restart_observations.push(json!({
        "phase": "restart_public_identity_before", "index": restarted,
        "offer_public": hex::encode(offer_public),
    }));
    world.state.joiner_offer_public_before_restart = Some(offer_public);
    restart_assert_live(world)
}

fn restart_public_state(
    world: &World,
    port: u16,
    height: u64,
    address: &str,
) -> Result<RestartPublicState> {
    let url = world.rpc.url(port);
    let address = address.parse::<alloy_primitives::Address>()?;
    let record = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::validatorByAddressCall { addr: address },
        height,
    )
    .map_err(|e| eyre!(e))?;
    ensure!(
        record.validatorAddress == address,
        "wrong restart validator identity"
    );
    let epoch = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getEpochNumberCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    let mut active = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    let mut participants = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveConsensusSetCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    let p2p = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getP2pAddressCall {
            validatorAddress: address,
        },
        height,
    )
    .map_err(|e| eyre!(e))?;
    active.sort_unstable();
    participants.sort_unstable();
    Ok(RestartPublicState {
        address,
        consensus_public_key: record.consensusPubkey,
        p2p_version: p2p.version,
        p2p_encoded: p2p.encoded,
        epoch: u64::try_from(epoch)?,
        status: record.status,
        stake: record.stake,
        active,
        participants,
        voter_misses: eth::read_call_at_result(
            &url,
            addresses::SLASH_ADDR,
            &eth::ISlashIndicator::getVoterMissCountCall { validator: address },
            height,
        )
        .map_err(|e| eyre!(e))?,
        supply: eth::read_call_at_result(
            &url,
            addresses::TRIBUTE_ADDR,
            &eth::ITribute::totalSupplyCall {},
            height,
        )
        .map_err(|e| eyre!(e))?,
    })
}

fn restart_same_registered_identity(
    before: &RestartPublicState,
    after: &RestartPublicState,
) -> Result<()> {
    ensure!(
        before.consensus_public_key.len() == 48,
        "missing canonical registered BLS identity"
    );
    ensure!(
        before.address == after.address
            && before.consensus_public_key == after.consensus_public_key
            && before.p2p_version == after.p2p_version
            && before.p2p_encoded == after.p2p_encoded,
        "restart changed registered BLS/P2P identity"
    );
    Ok(())
}

fn restart_snapshot(
    world: &mut World,
    checkpoint: FinalizedCheckpoint,
    address: &str,
    phase: &str,
) -> Result<RestartPublicState> {
    let mut agreed = None;
    for port in restart_ports(world)? {
        ensure!(
            world.rpc.finalized_result(port)? >= checkpoint.height,
            "restart snapshot is not finalized"
        );
        ensure!(
            world.rpc.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "restart checkpoint differs"
        );
        let state = restart_public_state(world, port, checkpoint.height, address)?;
        world.state.restart_observations.push(json!({
            "phase": phase, "port": port, "checkpoint": committee_checkpoint_json(checkpoint), "state": state,
        }));
        if let Some(before) = world.state.restart_observations.iter().find(|row| {
            row["phase"] == "restart_before" && row["state"]["address"] == json!(state.address)
        }) {
            let before: RestartPublicState = serde_json::from_value(before["state"].clone())?;
            restart_same_registered_identity(&before, &state)?;
        }
        if let Some(ref expected) = agreed {
            ensure!(
                &state == expected,
                "restart public state differs across expected peers"
            );
        } else {
            agreed = Some(state);
        }
        ensure!(
            world.rpc.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "restart checkpoint changed during reads"
        );
    }
    agreed.ok_or_else(|| eyre!("missing restart snapshot"))
}

fn restart_require_membership(world: &World, state: &RestartPublicState, status: u8) -> Result<()> {
    let mut expected = Vec::new();
    for index in 0..if status == 2 { 5 } else { 4 } {
        let key = world
            .validators
            .get(index)
            .evm_key()
            .map_err(|_| eyre!("validator identity unavailable"))?;
        expected.push(eth::address_of(&key).ok_or_else(|| eyre!("invalid validator identity"))?);
    }
    expected.sort_unstable();
    restart_validate_membership(state, status, &expected)
}

fn restart_validate_membership(
    state: &RestartPublicState,
    status: u8,
    expected: &[alloy_primitives::Address],
) -> Result<()> {
    ensure!(
        expected.len() == if status == 2 { 5 } else { 4 },
        "incomplete expected restart membership"
    );
    ensure!(
        state.status == status && state.active == expected && state.participants == expected,
        "restart changed exact expected membership or validator status"
    );
    if status == 0 {
        ensure!(state.stake.is_zero(), "registered restart acquired stake");
    }
    Ok(())
}

fn restart_pin_before(world: &mut World, status: u8, restarted: usize) -> Result<()> {
    restart_assert_live(world)?;
    let ports = restart_ports(world)?;
    let height = ports
        .iter()
        .map(|&port| world.rpc.finalized_result(port))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| eyre!("missing restart peers"))?;
    let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, height, 1)?;
    let address = world
        .state
        .joiner_addr
        .clone()
        .ok_or_else(|| eyre!("missing joiner identity"))?;
    let state = restart_snapshot(world, checkpoint, &address, "restart_before")?;
    restart_require_membership(world, &state, status)?;
    if status == 0 {
        ensure!(
            state.p2p_version == 1 && !state.p2p_encoded.is_empty(),
            "registered restart has no committed P2P identity"
        );
    }
    if restarted != 4 {
        let key = world
            .validators
            .get(restarted)
            .evm_key()
            .map_err(|_| eyre!("restart identity unavailable"))?;
        let address = world
            .rpc
            .address_of(&key)
            .ok_or_else(|| eyre!("invalid restarted public identity"))?;
        restart_snapshot(world, checkpoint, &address, "restart_before")?;
    }
    world.state.lifecycle_before = Some(checkpoint);
    world.state.marker_height = Some(checkpoint.height);
    world.state.marker_count = Some(usize::try_from(state.epoch)?);
    Ok(())
}

fn restart_fresh_checkpoint(world: &mut World, tries: u32) -> Result<FinalizedCheckpoint> {
    restart_assert_live(world)?;
    let ports = restart_ports(world)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(tries) * 3);
    let before = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing restart checkpoint"))?;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, before.height, tries)?;
    let target = world.rpc.fresh_finality_target(&ports)?;
    let checkpoint =
        world
            .rpc
            .wait_finalized_checkpoint(&ports, target, restart_remaining_tries(deadline)?)?;
    restart_assert_live(world)?;
    world.state.restart_observations.push(json!({
        "phase": "restart_fresh_finality", "ports": ports, "target": target,
        "checkpoint": committee_checkpoint_json(checkpoint),
    }));
    Ok(checkpoint)
}

fn restart_remaining_tries(deadline: std::time::Instant) -> Result<u32> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    ensure!(
        !remaining.is_zero(),
        "restart observation allowance expired"
    );
    Ok(u32::try_from(remaining.as_secs().div_ceil(3))?.max(1))
}

fn restart_finalize_hash(world: &mut World, hash: &str) -> Result<FinalizedCheckpoint> {
    restart_assert_live(world)?;
    let ports = restart_ports(world)?;
    let receipt = eth::raw_json_result(
        &world.rpc.url(ports[0]),
        "eth_getTransactionReceipt",
        json!([hash]),
    )?;
    // Retain the public observation before validation, including failed/null receipts.
    world.state.restart_observations.push(json!({
        "phase": "restart_receipt", "transaction_hash": hash, "receipt": receipt,
    }));
    let checkpoint = world.rpc.finalize_outcome(
        &TxOutcome {
            transaction_hash: hash.to_owned(),
            success: true,
            receipt,
        },
        &ports,
        60,
    )?;
    world.state.restart_observations.push(json!({
        "phase": "restart_receipt_finalized", "transaction_hash": hash,
        "ports": ports, "checkpoint": committee_checkpoint_json(checkpoint),
    }));
    restart_assert_live(world)?;
    Ok(checkpoint)
}

fn restart_replacement_matches(
    previous: (u32, u32),
    current: (u32, u32),
    enclave_replaced: bool,
) -> bool {
    current.0 != previous.0 && (current.1 != previous.1) == enclave_replaced
}

fn restart_sealed_node_evidence(
    index: usize,
    old: &mut RestartIncarnation,
) -> Result<serde_json::Value> {
    let log = old.node_log.read()?;
    Ok(json!({
        "phase": "restart_reaped_node_interval", "index": index, "node_pid": old.node_pid,
        "node_log_start": old.node_log.start_offset(),
        "rotation_started": log.contains("freezing validator set and starting DKG rotation"),
        "completed_before_activation": log.contains("persisted completed DKG state before activation"),
        "initial_ceremony": log.contains("running DKG ceremony"),
    }))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RestartFrozenRound {
    cycle: u64,
    freeze: u64,
    planned: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RestartFrozenTarget {
    round: RestartFrozenRound,
    // Commonware public-key order, as used by build_boundary_artifact.
    members: Vec<alloy_primitives::Address>,
    commitment: alloy_primitives::B256,
}

fn restart_inflight_round(log: &str) -> Result<RestartFrozenRound> {
    ensure!(
        !log.contains("persisted completed DKG state before activation")
            && !log.contains("VRF/DKG material activated"),
        "DKG completed before the owned in-flight fault was reaped"
    );
    let mut found = None;
    for line in log
        .lines()
        .filter(|line| line.contains("freezing validator set and starting DKG rotation"))
    {
        let number = |name: &str| -> Result<u64> {
            let prefix = format!("{name}=");
            let fields = line
                .split_whitespace()
                .filter_map(|part| part.strip_prefix(&prefix))
                .collect::<Vec<_>>();
            ensure!(
                fields.len() == 1,
                "missing or duplicated frozen-target field {name}"
            );
            fields[0]
                .parse()
                .map_err(|_| eyre!("malformed frozen-target field {name}"))
        };
        let round = RestartFrozenRound {
            cycle: number("dkg_cycle")?,
            freeze: number("freeze_height")?,
            planned: number("planned_activation_height")?,
        };
        ensure!(
            round.freeze <= round.planned,
            "invalid frozen-target schedule"
        );
        ensure!(
            found.as_ref().is_none_or(|previous| previous == &round),
            "in-flight restart observed more than one frozen target"
        );
        found = Some(round);
    }
    found.ok_or_else(|| eyre!("stopped incarnation has no frozen-target evidence"))
}

fn restart_target_commitment(
    round: &RestartFrozenRound,
    members: &[alloy_primitives::Address],
) -> alloy_primitives::B256 {
    // Exact public encoding in consensus::dkg_manager::hash_target_set (private
    // there): freeze BE64 || planned BE64 || count BE64 || ordered addresses.
    let mut bytes = Vec::with_capacity(24 + members.len() * 20);
    bytes.extend_from_slice(&round.freeze.to_be_bytes());
    bytes.extend_from_slice(&round.planned.to_be_bytes());
    bytes.extend_from_slice(&(members.len() as u64).to_be_bytes());
    for address in members {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

fn restart_retain_frozen_target(
    world: &mut World,
    stopped: usize,
    round: RestartFrozenRound,
) -> Result<()> {
    let before = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing pre-fault checkpoint"))?;
    ensure!(
        round.freeze <= before.height,
        "frozen target was not in the pre-fault finalized history"
    );
    // Read the immutable pre-fault history on every survivor. This deliberately
    // happens after the fault, not as a DKG-completion warmup before it.
    let mut agreed = None;
    for index in (0..5).filter(|index| *index != stopped) {
        let port = world.validators.http_port(index);
        ensure!(
            world.rpc.finalized_result(port)? >= before.height
                && world.rpc.checkpoint_at(port, before.height)? == before,
            "survivor changed pre-fault canonical history"
        );
        let freeze = world.rpc.checkpoint_at(port, round.freeze)?;
        let mut keys = Vec::new();
        for member in 0..5 {
            let key = world
                .validators
                .get(member)
                .evm_key()
                .map_err(|_| eyre!("validator identity unavailable"))?;
            let address =
                eth::address_of(&key).ok_or_else(|| eyre!("invalid public validator identity"))?;
            let record = eth::read_call_at_result(
                &world.rpc.url(port),
                addresses::VS_ADDR,
                &eth::IValidatorSet::validatorByAddressCall { addr: address },
                round.freeze,
            )
            .map_err(|e| eyre!(e))?;
            ensure!(
                record.validatorAddress == address
                    && record.status == if member == 4 { 1 } else { 2 },
                "freeze checkpoint does not contain the expected four ACTIVE plus PENDING joiner"
            );
            let public_key = <commonware_cryptography::bls12381::PublicKey as commonware_codec::DecodeExt<()>>::decode(
                record.consensusPubkey.as_ref()).map_err(|_| eyre!("invalid public BLS identity at freeze"))?;
            keys.push((public_key, address));
        }
        keys.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        ensure!(
            keys.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "duplicate frozen BLS identity"
        );
        let members = keys
            .into_iter()
            .map(|(_, address)| address)
            .collect::<Vec<_>>();
        let target = RestartFrozenTarget {
            commitment: restart_target_commitment(&round, &members),
            round: round.clone(),
            members,
        };
        world.state.restart_observations.push(json!({
            "phase": "restart_frozen_target", "port": port, "stopped_index": stopped,
            "pre_fault_checkpoint": committee_checkpoint_json(before),
            "freeze_checkpoint": committee_checkpoint_json(freeze), "target": target,
        }));
        if let Some(ref previous) = agreed {
            ensure!(
                previous == &(freeze, target.clone()),
                "survivors disagree on frozen commitment"
            );
        } else {
            agreed = Some((freeze, target));
        }
        ensure!(
            world.rpc.checkpoint_at(port, round.freeze)? == freeze,
            "frozen checkpoint changed during reads"
        );
    }
    Ok(())
}

fn restart_validate_frozen_target(
    expected: &RestartFrozenTarget,
    round: &RestartFrozenRound,
    commitment: alloy_primitives::B256,
    members: &[alloy_primitives::Address],
    activation: u64,
) -> Result<()> {
    ensure!(
        &expected.round == round
            && expected.commitment == commitment
            && expected.members == members
            && activation >= round.planned,
        "admission does not match the interrupted frozen cycle/commitment"
    );
    Ok(())
}

fn restart_install_replacement(
    world: &mut World,
    index: usize,
    previous: (u32, u32),
    node_log: LaunchLog,
    enclave_log: LaunchLog,
    enclave_replaced: bool,
) -> Result<()> {
    let (node_pid, enclave_pid) = world.localnet.live_validator_and_enclave_pids(index)?;
    ensure!(
        restart_replacement_matches(previous, (node_pid, enclave_pid), enclave_replaced),
        "restart did not replace exactly the requested owned children"
    );
    let keys_dir = if index == 4 {
        std::path::PathBuf::from(world.localnet.keys_dir(index))
    } else {
        world
            .localnet
            .scenario_dir()
            .join(format!("validator-{index}/data/keys"))
    };
    world.state.restart_observations.push(json!({
        "phase": "restart_replaced", "index": index, "before_pids": previous,
        "node_pid": node_pid, "enclave_pid": enclave_pid,
        "node_log_start": node_log.start_offset(), "enclave_log_start": enclave_log.start_offset(),
        "keys_dir": keys_dir,
    }));
    world.state.lifecycle_incarnations.insert(
        index,
        RestartIncarnation {
            node_pid,
            enclave_pid,
            node_log,
            enclave_log,
        },
    );
    restart_assert_live(world)?;
    let offer_public = world.localnet.node_offer_public(index)?;
    world.state.restart_observations.push(json!({
        "phase": "restart_public_identity_after", "index": index,
        "offer_public": hex::encode(offer_public),
        "before_offer_public": world.state.joiner_offer_public_before_restart.map(hex::encode),
    }));
    ensure!(
        Some(offer_public) == world.state.joiner_offer_public_before_restart,
        "restart changed the authenticated permanent public offer key"
    );
    Ok(())
}

fn restart_joiner_pair(world: &mut World, index: usize, in_flight: bool) -> Result<()> {
    restart_assert_live(world)?;
    let mut old = world
        .state
        .lifecycle_incarnations
        .remove(&index)
        .ok_or_else(|| eyre!("missing restart owner"))?;
    let previous = (old.node_pid, old.enclave_pid);
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{index}"));
    let keys = world.localnet.keys_dir(index);
    let expected_round = if in_flight {
        Some(restart_inflight_round(&old.node_log.read()?)?)
    } else {
        None
    };
    if let Some(round) = &expected_round {
        world.state.restart_observations.push(json!({
            "phase": "restart_inflight_armed", "index": index, "node_pid": old.node_pid, "round": round,
        }));
    }
    world.localnet.stop_joiner(index)?;
    old.node_log.seal()?;
    world
        .state
        .restart_observations
        .push(restart_sealed_node_evidence(index, &mut old)?);
    if let Some(expected_round) = expected_round {
        let round = restart_inflight_round(&old.node_log.read()?)?;
        ensure!(
            round == expected_round,
            "frozen target changed while stopping the in-flight node"
        );
        restart_retain_frozen_target(world, index, round)?;
    }
    let node_log = LaunchLog::arm(&dir.join("node.log"))?;
    let mut enclave_log = None;
    world.localnet.restart_joiner_enclave_observed(index, |_| {
        old.enclave_log.seal()?;
        enclave_log = Some(LaunchLog::arm(&dir.join("enclave.log"))?);
        Ok(())
    })?;
    ensure!(
        world.localnet.keys_dir(index) == keys,
        "restart changed keys directory"
    );
    world
        .localnet
        .launch_joiner(index, &["--consensus.keys-dir", &keys])?;
    restart_install_replacement(
        world,
        index,
        previous,
        node_log,
        enclave_log.ok_or_else(|| eyre!("enclave restart interval was not armed"))?,
        true,
    )
}

fn restart_check_logs(world: &mut World, index: usize, unseal: bool, dealer: bool) -> Result<()> {
    restart_assert_live(world)?;
    let owner = world
        .state
        .lifecycle_incarnations
        .get_mut(&index)
        .ok_or_else(|| eyre!("missing restart log owner"))?;
    let node = owner.node_log.read()?;
    let enclave = owner.enclave_log.read()?;
    let initial_ceremony = node.contains("running DKG ceremony");
    let unsealed = enclave.contains("unsealed offer key + group signature");
    let restored_dealer = node.contains("restoring durable DKG dealer transcript");
    world.state.restart_observations.push(json!({
        "phase": "restart_launch_markers", "index": index, "initial_ceremony": initial_ceremony,
        "unsealed": unsealed, "restored_dealer": restored_dealer,
        "node_pid": owner.node_pid, "enclave_pid": owner.enclave_pid,
    }));
    restart_require_markers(&node, &enclave, unseal, dealer)?;
    for incarnation in world.state.lifecycle_incarnations.values_mut() {
        ensure!(
            !incarnation
                .node_log
                .read()?
                .contains("byzantine evidence observed"),
            "byzantine/equivocation evidence during restart interval"
        );
    }
    Ok(())
}

fn restart_require_markers(node: &str, enclave: &str, unseal: bool, dealer: bool) -> Result<()> {
    ensure!(
        !node.contains("running DKG ceremony"),
        "restart invoked initial-genesis DKG instead of recovering current material"
    );
    ensure!(
        !unseal || enclave.contains("unsealed offer key + group signature"),
        "replacement enclave has no launch-scoped unseal evidence"
    );
    ensure!(
        !dealer || node.contains("restoring durable DKG dealer transcript"),
        "replacement dealer has no launch-scoped restoration evidence"
    );
    ensure!(
        !node.contains("byzantine evidence observed"),
        "byzantine evidence in restarted incarnation"
    );
    Ok(())
}

// A rotation invalidates a counter comparison, not the recovery. The caller may
// choose a new fully eligible window within the ORIGINAL remaining allowance.
fn restart_signing_comparison(
    before_height: u64,
    after_height: u64,
    before_epoch: u64,
    after_epoch: u64,
    before_misses: u64,
    after_misses: u64,
) -> Result<bool> {
    let target = before_height
        .checked_add(5)
        .and_then(|h| h.checked_add(LATE_FINALIZE_WINDOW_K))
        .ok_or_else(|| eyre!("signing target overflow"))?;
    ensure!(
        after_height >= target && after_epoch >= before_epoch,
        "incomplete or regressed signing window"
    );
    if after_epoch != before_epoch {
        return Ok(false);
    }
    ensure!(
        after_misses == before_misses,
        "recovered eligible validator accumulated voter misses"
    );
    Ok(true)
}

fn restart_prove_signing(
    world: &mut World,
    address: &str,
    drain_tries: u32,
    window_tries: u32,
) -> Result<FinalizedCheckpoint> {
    restart_assert_live(world)?;
    let ports = restart_ports(world)?;
    let drain_deadline =
        std::time::Instant::now() + Duration::from_secs(u64::from(drain_tries) * 3);
    let original = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing restart checkpoint"))?;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, original.height, drain_tries)?;
    let fresh = world.rpc.fresh_finality_target(&ports)?;
    let drain = fresh
        .checked_add(LATE_FINALIZE_WINDOW_K)
        .ok_or_else(|| eyre!("eligibility target overflow"))?;
    let mut before = world.rpc.wait_finalized_checkpoint(
        &ports,
        drain,
        restart_remaining_tries(drain_deadline)?,
    )?;
    let mut baseline = restart_snapshot(world, before, address, "restart_signing_baseline")?;
    restart_require_membership(world, &baseline, 2)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(window_tries) * 3);
    loop {
        restart_assert_live(world)?;
        ensure!(
            std::time::Instant::now() < deadline,
            "fully closed eligible signing window did not finalize"
        );
        let target = before
            .height
            .checked_add(5)
            .and_then(|h| h.checked_add(LATE_FINALIZE_WINDOW_K))
            .ok_or_else(|| eyre!("signing target overflow"))?;
        let observed = ports
            .iter()
            .map(|&port| world.rpc.finalized_result(port))
            .collect::<Result<Vec<_>>>()?;
        if observed.iter().any(|&height| height < target) {
            sleep(Duration::from_secs(3));
            continue;
        }
        let after = world.rpc.wait_finalized_checkpoint(&ports, target, 1)?;
        let state = restart_snapshot(world, after, address, "restart_signing_closed")?;
        restart_require_membership(world, &state, 2)?;
        if restart_signing_comparison(
            before.height,
            after.height,
            baseline.epoch,
            state.epoch,
            baseline.voter_misses,
            state.voter_misses,
        )? {
            restart_assert_live(world)?;
            return Ok(after);
        }
        // A new boundary can itself have delayed pre-eligibility accounting.
        // Drain it before choosing another window, without resetting the deadline.
        let drained = after
            .height
            .checked_add(LATE_FINALIZE_WINDOW_K)
            .ok_or_else(|| eyre!("rotation drain overflow"))?;
        before = world.rpc.wait_finalized_checkpoint(
            &ports,
            drained,
            restart_remaining_tries(deadline)?,
        )?;
        baseline = restart_snapshot(world, before, address, "restart_signing_rotation_drain")?;
        restart_require_membership(world, &baseline, 2)?;
    }
}

fn restart_boundary_transition(
    previous_epoch: u64,
    next_epoch: u64,
    boundary_epoch: Option<u64>,
    was_active: bool,
    now_active: bool,
    frozen: bool,
) -> Result<bool> {
    ensure!(!was_active || now_active, "restart admission was reversed");
    if let Some(epoch) = boundary_epoch {
        ensure!(
            previous_epoch.checked_add(1) == Some(epoch) && next_epoch == epoch,
            "duplicate, skipped, or mismatched canonical boundary epoch"
        );
        ensure!(
            was_active || now_active || !frozen,
            "frozen target boundary omitted the joiner"
        );
    } else {
        ensure!(
            next_epoch == previous_epoch && was_active == now_active,
            "partial activation or epoch transition without a canonical boundary"
        );
    }
    Ok(!was_active && now_active)
}

fn restart_activation(world: &mut World, frozen: bool) -> Result<FinalizedCheckpoint> {
    let target: Option<RestartFrozenTarget> = if frozen {
        let row = world
            .state
            .restart_observations
            .iter()
            .find(|row| row["phase"] == "restart_frozen_target")
            .ok_or_else(|| eyre!("missing interrupted frozen commitment"))?;
        Some(serde_json::from_value(row["target"].clone())?)
    } else {
        None
    };
    let address = world
        .state
        .joiner_addr
        .clone()
        .ok_or_else(|| eyre!("missing joining identity"))?;
    ensure!(
        world
            .rpc
            .wait_participant(world.validators.primary_port(), &address, 90)?,
        "joining validator did not activate after restart"
    );
    let after = restart_fresh_checkpoint(world, 90)?;
    let before = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing pre-restart checkpoint"))?;
    let old_epoch = restart_public_state(
        world,
        world.validators.primary_port(),
        before.height,
        &address,
    )?
    .epoch;
    let mut previous_epoch = old_epoch;
    let mut activated = None;
    for height in before
        .height
        .checked_add(1)
        .ok_or_else(|| eyre!("boundary scan overflow"))?..=after.height
    {
        let artifact = pending_boundary_at(&world.rpc, world.validators.primary_port(), height)?;
        let state = restart_public_state(world, world.validators.primary_port(), height, &address)?;
        let now_active = state.status == 2;
        restart_require_membership(world, &state, if now_active { 2 } else { 1 })?;
        let admitted = restart_boundary_transition(
            previous_epoch,
            state.epoch,
            artifact.as_ref().map(|a| a.epoch),
            activated.is_some(),
            now_active,
            frozen,
        )?;
        if let Some(artifact) = artifact {
            let checkpoint = world
                .rpc
                .checkpoint_at(world.validators.primary_port(), height)?;
            restart_snapshot(world, checkpoint, &address, "restart_canonical_boundary")?;
            world.state.restart_observations.push(json!({
                "phase": "restart_boundary", "checkpoint": committee_checkpoint_json(checkpoint),
                "epoch": artifact.epoch, "dkg_cycle": artifact.dkg_cycle, "admitted": admitted,
                "freeze_height": artifact.freeze_height, "planned_activation_height": artifact.planned_activation_height,
                "target_set_hash": artifact.target_set_hash, "new_active_set": artifact.reshare.new_active_set,
                "outcome_hash": alloy_primitives::keccak256(&artifact.outcome),
            }));
            if let Some(target) = &target {
                ensure!(
                    artifact.dkg_cycle != target.round.cycle || admitted,
                    "interrupted frozen target activated more than once or without admission"
                );
                if admitted {
                    ensure!(
                        artifact.is_validator_set_change && !artifact.is_full_dkg,
                        "interrupted reshare was replaced by a different ceremony mode"
                    );
                    restart_validate_frozen_target(
                        target,
                        &RestartFrozenRound {
                            cycle: artifact.dkg_cycle,
                            freeze: artifact.freeze_height,
                            planned: artifact.planned_activation_height,
                        },
                        artifact.target_set_hash,
                        &artifact.reshare.new_active_set,
                        height,
                    )?;
                }
            }
            if admitted {
                activated = Some(checkpoint);
            }
        }
        previous_epoch = state.epoch;
    }
    let state = restart_snapshot(world, after, &address, "restart_activated_terminal")?;
    restart_require_membership(world, &state, 2)?;
    ensure!(
        state.epoch == previous_epoch,
        "terminal epoch differs from canonical boundary history"
    );
    activated.ok_or_else(|| eyre!("restart recovery has no canonical admission boundary"))
}

fn wait_for_dkg_retry_snapshot(keys_dir: impl AsRef<Path>, validator: usize, file: &str) {
    let snapshot = keys_dir.as_ref().join(file);
    for _ in 0..1_800 {
        if snapshot.exists() {
            return;
        }
        sleep(Duration::from_millis(100));
    }
    panic!("validator-{validator} did not persist {file} before the restart crash point");
}

/// Bring a joiner to ACTIVE with a persisted (keys-dir) share (s4:13-30).
#[when("a joiner reaches active with a persisted share")]
fn joiner_active_persisted_share(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let wwd = world.state.wwd.clone().expect("wwd");
    let v0 = world.validators.get(0).evm_key().expect("v0 key");
    world.state.tribute_tx_hash = Some(
        world
            .rpc
            .offer_until_supply_hash(&v0, &wwd, primary, "1", 5)
            .expect("pre-restart offer did not land"),
    );
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner identity");
    world.state.joiner_addr = Some(addr.clone());
    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    world.rpc.confirm_ready(&key).expect("confirm ready");
    assert!(
        world
            .rpc
            .wait_participant(primary, &addr, 70)
            .expect("observable participation"),
        "joiner did not activate before restart"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "share was not persisted before restart"
    );
    sleep(Duration::from_secs(20));
}

/// Kill only the node (enclave container stays up) and restart it with the same
/// keys-dir/datadir (s4:32-37).
#[when("the node is killed and restarted with the same keys")]
fn node_killed_and_restarted(world: &mut World) {
    let idx = world.validators.joiner_index();
    restart_capture_incarnations(world, idx).expect("capture exact pre-fault owners");
    // The primary has not restarted in this scenario. Preserve the original
    // whole-incarnation negative, then cover subsequent output with its interval.
    assert_eq!(
        world
            .localnet
            .log_count(0, "byzantine evidence observed")
            .expect("read owned primary log"),
        0,
        "pre-fault byzantine/equivocation evidence"
    );
    restart_pin_before(world, 2, idx).expect("canonical ACTIVE pre-fault checkpoint");
    let offer = world
        .state
        .tribute_tx_hash
        .clone()
        .expect("pre-restart offer");
    restart_finalize_hash(world, &offer).expect("pre-restart offer finalized on all five nodes");
    let mut old = world
        .state
        .lifecycle_incarnations
        .remove(&idx)
        .expect("owned restart target");
    let previous = (old.node_pid, old.enclave_pid);
    let status = world
        .localnet
        .kill_validator_owned(idx, old.node_pid)
        .expect("fault exact owned node");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "node-only restart did not observe the requested SIGKILL"
    );
    world.state.restart_observations.push(json!({
        "phase": "restart_node_fault", "index": idx, "node_pid": old.node_pid,
        "enclave_pid": old.enclave_pid, "exit_signal": 9,
    }));
    old.node_log.seal().expect("seal old node incarnation");
    world.state.restart_observations.push(
        restart_sealed_node_evidence(idx, &mut old).expect("retain sealed old node observation"),
    );
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{idx}"));
    let node_log = LaunchLog::arm(&dir.join("node.log")).expect("arm replacement node");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("restart same node slot");
    restart_install_replacement(world, idx, previous, node_log, old.enclave_log, false)
        .expect("only node replaced; enclave and public identity preserved");
}

/// Stop every committee node and enclave, then relaunch them from the same
/// datadirs. Each identity must unseal its own permanent key; there is no peer
/// redelivery path.
#[when("the entire committee and its enclaves are stopped and restarted")]
fn committee_and_enclaves_restarted(world: &mut World) {
    let ports = world.validators.committee_ports();
    let height = ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_result(port)
                .expect("pre-restart finality")
        })
        .min()
        .expect("nonempty committee");
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 1)
        .expect("common pre-restart finalized hash/root");
    let original_pids: Vec<_> = (0..world.validators.size())
        .map(|index| {
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("both original committee processes must be owned and live")
        })
        .collect();
    assert!(
        world.state.committee_restart.is_empty(),
        "committee restart already armed"
    );
    world.state.marker_height = Some(before.height);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_restart",
        "checkpoint": committee_checkpoint_json(before),
        "original_pids": original_pids,
    }));
    let mut logs = Vec::new();
    world
        .localnet
        .restart_committee_and_enclaves_observed(|stopped| {
            // Old processes have been reaped; no replacement has started yet.
            for index in 0..original_pids.len() {
                let dir = stopped.scenario_dir().join(format!("validator-{index}"));
                logs.push((
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("node.log"))?,
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("enclave.log"))?,
                ));
            }
            Ok(())
        })
        .expect("restart committee and enclaves");
    for (index, (node_log, enclave_log)) in logs.into_iter().enumerate() {
        let (node_pid, enclave_pid) = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("both replacement committee processes must be owned and live");
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_replacement",
            "validator": index,
            "node_pid": node_pid,
            "enclave_pid": enclave_pid,
            "node_log_start": node_log.start_offset(),
            "enclave_log_start": enclave_log.start_offset(),
        }));
        assert_ne!(node_pid, original_pids[index].0, "node was not replaced");
        assert_ne!(
            enclave_pid, original_pids[index].1,
            "enclave was not replaced"
        );
        world
            .state
            .committee_restart
            .push(crate::world::state::RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            });
    }
}

/// Every enclave must use its restart fast-path, every validator must advance,
/// and an enclave-backed Tribute offer must remain executable.
#[then("all validators recover sealed TEE state and resume finalization")]
fn committee_recovers_sealed_tee_state(world: &mut World) {
    let before = world.state.marker_height.expect("pre-restart height");
    let ports = world.validators.committee_ports();
    committee_assert_live(world);
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("fresh target from every restarted validator")
        .max(before.checked_add(2).expect("restart height overflow"));
    let progressed = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("restarted committee must advance on one finalized hash/root");
    committee_assert_live(world);
    let before_checkpoint = world
        .state
        .restart_observations
        .iter()
        .rev()
        .find(|observation| observation["phase"] == "committee_before_restart")
        .expect("retained pre-restart checkpoint")["checkpoint"]
        .clone();
    for &port in &ports {
        assert_eq!(
            committee_checkpoint_json(
                world
                    .rpc
                    .checkpoint_at(port, before)
                    .expect("pre-restart finalized block must remain canonical")
            ),
            before_checkpoint,
            "validator RPC {port} changed the pre-restart finalized hash/root"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_restarted_finality",
        "target": target,
        "checkpoint": committee_checkpoint_json(progressed),
    }));
    for index in 0..world.validators.size() {
        let incarnation = &mut world.state.committee_restart[index];
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let unsealed: Vec<_> = enclave_log.lines().filter(|line| {
            *line == "outbe-tee-enclave: unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)"
        }).collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_unsealed",
            "validator": index,
            "enclave_pid": incarnation.enclave_pid,
            "records": unsealed,
        }));
        assert_eq!(
            unsealed.len(),
            1,
            "validator-{index} enclave did not recover its sealed offer key"
        );
    }
    let wwd = world.state.wwd.clone().expect("wwd");
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let primary = world.validators.primary_port();
    let supply_before = committee_supply_at(world, primary, progressed);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, progressed),
            supply_before,
            "pre-offer finalized supply parity"
        );
    }
    let expected_supply = supply_before
        .checked_add(alloy_primitives::U256::from(1))
        .expect("Tribute supply overflow");
    // Retain an exact prefix of the existing launch capture. The suffix must
    // come from these same processes after this checkpoint, not earlier replay.
    let offer_prefixes: Vec<_> = world
        .state
        .committee_restart
        .iter_mut()
        .map(|incarnation| {
            incarnation
                .enclave_log
                .read()
                .expect("checkpoint replacement enclave log before the new offer")
        })
        .collect();
    committee_assert_live(world);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_new_offer",
        "checkpoint": committee_checkpoint_json(progressed),
        "supply": supply_before.to_string(),
        "expected_supply": expected_supply.to_string(),
        "enclave_log_starts": world.state.committee_restart.iter().zip(&offer_prefixes)
            .map(|(incarnation, prefix)| incarnation.enclave_log.start_offset()
                .checked_add(u64::try_from(prefix.len()).expect("log length fits u64"))
                .expect("offer log offset overflow")).collect::<Vec<_>>(),
    }));
    // This helper submits a real offer before polling, even if supply is already visible.
    let transaction_hash = world
        .rpc
        .offer_until_supply_hash(&key, &wwd, primary, &expected_supply.to_string(), 5)
        .expect("new post-restart Tribute offer must be submitted and included");
    let receipt = crate::internal::eth::receipt_json(&world.rpc.url(primary), &transaction_hash)
        .expect("new Tribute transaction receipt must be observable");
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_receipt",
        "transaction_hash": transaction_hash,
        "receipt": receipt,
    }));
    let outcome = crate::world::rpc::TxOutcome {
        transaction_hash,
        success: receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1"),
        receipt,
    };
    assert!(outcome.success, "new Tribute offer reverted");
    assert!(
        outcome.block_number().expect("Tribute receipt height") > progressed.height,
        "Tribute receipt predates post-restart observation"
    );
    let finalized = world.rpc.finalize_outcome(&outcome, &ports, 60).expect(
        "new successful Tribute receipt must be canonical and finalized on every validator",
    );
    committee_assert_live(world);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, finalized),
            expected_supply,
            "new finalized Tribute must increase supply by exactly one"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_finalized",
        "transaction_hash": outcome.transaction_hash,
        "checkpoint": committee_checkpoint_json(finalized),
        "supply_before": supply_before.to_string(),
        "supply_after": expected_supply.to_string(),
    }));
    for (index, prefix) in offer_prefixes.iter().enumerate() {
        let incarnation = &mut world.state.committee_restart[index];
        incarnation
            .node_log
            .seal()
            .expect("seal replacement node observation");
        incarnation
            .enclave_log
            .seal()
            .expect("seal replacement enclave observation");
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let offer_log = enclave_log
            .strip_prefix(prefix.as_str())
            .expect("pre-offer launch log prefix changed");
        let decrypted: Vec<_> = offer_log
            .lines()
            .filter(|line| {
                line.starts_with("outbe-tee-enclave: req=process_tribute_offer_batch ")
                    && line
                        .split_ascii_whitespace()
                        .filter(|field| field.starts_with("outcome="))
                        .eq(std::iter::once("outcome=ok"))
            })
            .collect();
        let node_log = incarnation
            .node_log
            .read()
            .expect("replacement node log identity");
        let ceremonies: Vec<_> = node_log
            .lines()
            .filter(|line| line.contains("running DKG ceremony"))
            .collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_restart_served_new_offer",
            "validator": index,
            "node_pid": incarnation.node_pid,
            "enclave_pid": incarnation.enclave_pid,
            "decrypt_records": decrypted,
            "new_ceremony_records": ceremonies,
        }));
        assert!(
            !decrypted.is_empty(),
            "validator-{index} lacks a successful new-offer decrypt"
        );
        assert!(
            ceremonies.is_empty(),
            "validator-{index} restart triggered a fresh DKG ceremony"
        );
    }
    committee_assert_live(world);
}

fn committee_assert_live(world: &mut World) {
    assert_eq!(
        world.state.committee_restart.len(),
        world.validators.size(),
        "every committee replacement must be observed"
    );
    for index in 0..world.validators.size() {
        let observed = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("committee replacement processes must remain owned and live");
        let expected = &world.state.committee_restart[index];
        assert_eq!(
            observed,
            (expected.node_pid, expected.enclave_pid),
            "validator-{index} changed process incarnation during restart proof"
        );
    }
}

fn committee_checkpoint_json(
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> serde_json::Value {
    serde_json::json!({
        "height": checkpoint.height,
        "block_hash": format!("{:#x}", checkpoint.block_hash),
        "state_root": format!("{:#x}", checkpoint.state_root),
    })
}

fn committee_supply_at(
    world: &World,
    port: u16,
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> alloy_primitives::U256 {
    assert!(
        world
            .rpc
            .finalized_result(port)
            .expect("supply observation finality")
            >= checkpoint.height
    );
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("supply checkpoint"),
        checkpoint
    );
    let supply = crate::internal::eth::read_call_at_result(
        &world.rpc.url(port),
        crate::internal::addresses::TRIBUTE_ADDR,
        &crate::internal::eth::ITribute::totalSupplyCall {},
        checkpoint.height,
    )
    .expect("read Tribute supply at exact finalized checkpoint");
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("recheck supply checkpoint"),
        checkpoint
    );
    supply
}

/// The restarted node catches up and resumes signing WITHOUT a fresh ceremony
/// (s4:38-55).
#[then("it resumes signing from the persisted share without a new ceremony")]
fn resumes_without_new_ceremony(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    // Retain the original 30x3s catch-up and 30x2s lockstep allowances.
    let after = restart_prove_signing(world, &addr, 30, 20)
        .expect("all-five finalized eligible signing proof");
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "restarted validator has no current private threshold share"
    );
    assert!(
        world.localnet.has_share_file(idx),
        "persisted signing share disappeared"
    );
    restart_check_logs(world, idx, false, false)
        .expect("persisted-share startup without genesis DKG");
    let state = restart_snapshot(world, after, &addr, "persisted_share_restart_complete")
        .expect("typed pinned supply and membership parity");
    restart_require_membership(world, &state, 2).expect("same five ACTIVE identities");
    assert_eq!(
        state.supply,
        alloy_primitives::U256::from(1),
        "pre-restart Tribute supply changed"
    );
}

/// Complete a 4->5 DKG while the joiner is still PENDING, leaving a durable
/// recovery checkpoint and a real block interval before activation.
#[when("a joiner completes DKG and waits below the activation boundary")]
fn joiner_completes_dkg_before_activation(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(addr.clone());
    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(idx));
    let stake = world.rpc.stake(&key, 1000).expect("stake joiner");
    pending_finalize_transaction(&world.rpc, &stake, &ports)
        .expect("joiner stake must be canonically finalized");
    let ready = world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let admission = pending_finalize_transaction(&world.rpc, &ready, &ports)
        .expect("joiner readiness must be canonically finalized");

    let mut observed = false;
    for _ in 0..90 {
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("owned joiner and enclave must remain alive before the crash point");
        if Path::new(&keys)
            .join("dkg_pending_boundary.bin")
            .try_exists()
            .expect("observe installed pending DKG snapshot")
        {
            observed = true;
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(observed, "joiner never reached durable pending DKG state");
    let public_key = world
        .localnet
        .consensus_public_key(idx)
        .expect("read provisioned consensus public key");
    let public_key = hex::decode(public_key.trim().trim_start_matches("0x"))
        .expect("decode consensus public key");
    let checkpoint = PendingDkgCheckpoint::observe(Path::new(&keys), &public_key)
        .expect("pending triplet must match the installed boundary and this joiner");
    let before = world
        .rpc
        .wait_finalized_checkpoint(
            &ports,
            admission.height.max(checkpoint.completed_at_height),
            60,
        )
        .expect("all nodes must share a finalized pre-restart checkpoint");
    assert!(
        before.height < checkpoint.artifact.planned_activation_height,
        "missed completed-but-pending crash point: activation boundary already reached"
    );
    let old_epoch = checkpoint
        .artifact
        .epoch
        .checked_sub(1)
        .expect("incoming DKG epoch");
    for &port in &ports {
        pending_assert_membership(&world.rpc, port, &addr, before.height, 1, false, old_epoch)
            .expect("joiner must remain PENDING at the common finalized checkpoint");
    }
    let original_pids = world
        .localnet
        .live_validator_and_enclave_pids(idx)
        .expect("capture owned live node and enclave before restart");
    world.state.restart_observations.push(json!({
        "phase": "completed_pending", "slot": idx, "height": before.height,
        "block_hash": before.block_hash, "state_root": before.state_root,
        "stake_tx": stake, "ready_tx": ready, "node_pid": original_pids.0,
        "enclave_pid": original_pids.1, "epoch": checkpoint.artifact.epoch,
        "dkg_cycle": checkpoint.artifact.dkg_cycle,
        "completed_at_height": checkpoint.completed_at_height,
        "planned_activation_height": checkpoint.artifact.planned_activation_height,
        "target_set_hash": checkpoint.artifact.target_set_hash,
        "outcome_hash": alloy_primitives::keccak256(&checkpoint.artifact.outcome)
    }));
    world.state.pending_dkg_restart = Some(PendingDkgRestartState {
        checkpoint,
        keys_dir: keys.into(),
        consensus_public_key: public_key,
        before,
        original_pids,
        replacement: None,
    });
}

fn pending_finalize_transaction(
    rpc: &Rpc,
    hash: &str,
    ports: &[u16],
) -> Result<FinalizedCheckpoint> {
    let port = *ports.first().ok_or_else(|| eyre!("missing receipt RPC"))?;
    let receipt = eth::raw_json_result(&rpc.url(port), "eth_getTransactionReceipt", json!([hash]))?;
    rpc.finalize_outcome(
        &TxOutcome {
            transaction_hash: hash.to_owned(),
            success: true,
            receipt,
        },
        ports,
        60,
    )
}

fn pending_assert_membership(
    rpc: &Rpc,
    port: u16,
    addr: &str,
    height: u64,
    status: u8,
    participant: bool,
    epoch: u64,
) -> Result<()> {
    let record = rpc.validator_record_at(port, addr, height).ok_or_else(|| {
        eyre!("cannot observe validator record at finalized h{height} on RPC {port}")
    })?;
    ensure!(
        record.status == status,
        "unexpected validator status at finalized h{height}"
    );
    let actual = eth::read_call_at_result(
        &rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::isConsensusParticipantCall {
            addr: addr.parse()?,
        },
        height,
    )
    .map_err(|error| eyre!(error))?;
    ensure!(
        actual == participant,
        "unexpected participation at finalized h{height}"
    );
    let actual = eth::read_call_at_result(
        &rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::getEpochNumberCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    ensure!(
        u64::try_from(actual)? == epoch,
        "unexpected epoch at finalized h{height}"
    );
    Ok(())
}

/// Restart both halves of the joining validator while the finalized DKG result
/// is durable but has not yet become the active committee.
#[when("the joining node and enclave restart before activation")]
fn restart_joiner_before_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let mut state = world
        .state
        .pending_dkg_restart
        .take()
        .expect("pending DKG checkpoint");
    let height = world
        .rpc
        .finalized_result(primary)
        .expect("pre-stop finalized height");
    assert!(
        height >= state.before.height
            && height < state.checkpoint.artifact.planned_activation_height,
        "completed-but-pending restart window was missed"
    );
    pending_assert_membership(
        &world.rpc,
        primary,
        &addr,
        height,
        1,
        false,
        state
            .checkpoint
            .artifact
            .epoch
            .checked_sub(1)
            .expect("incoming epoch"),
    )
    .expect("finalized state must still be pending immediately before stop");
    assert_eq!(
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("owned original processes"),
        state.original_pids,
        "joiner incarnation changed before the intended fault"
    );

    world.localnet.stop_joiner(idx).expect("stop joiner");
    assert_eq!(
        PendingDkgCheckpoint::observe(&state.keys_dir, &state.consensus_public_key)
            .expect("pending checkpoint survives node stop"),
        state.checkpoint,
        "a different ceremony was persisted before restart"
    );
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{idx}"));
    let node_log = LaunchLog::arm(&dir.join("node.log")).expect("arm replacement node log");
    let mut enclave_log = None;
    let mut stopped_height = None;
    let rpc = &world.rpc;
    world
        .localnet
        .restart_joiner_enclave_observed(idx, |_| {
            let observed = rpc.finalized_result(primary)?;
            ensure!(
                observed < state.checkpoint.artifact.planned_activation_height,
                "completed-but-pending fault window was crossed before both launchers stopped"
            );
            stopped_height = Some(observed);
            enclave_log = Some(LaunchLog::arm(&dir.join("enclave.log"))?);
            Ok(())
        })
        .expect("restart joiner enclave");
    let enclave_log = enclave_log.expect("replacement enclave interval armed after teardown");
    let keys = world.localnet.keys_dir(idx);
    assert_eq!(
        Path::new(&keys),
        state.keys_dir,
        "restart must preserve keys directory"
    );
    world
        .localnet
        .launch_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("restart joiner node");
    let (node_pid, enclave_pid) = world
        .localnet
        .live_validator_and_enclave_pids(idx)
        .expect("replacement node and enclave must be alive");
    assert_ne!(node_pid, state.original_pids.0, "node was not restarted");
    assert_ne!(
        enclave_pid, state.original_pids.1,
        "enclave was not restarted"
    );
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_restart",
        "slot": idx, "node_pid": node_pid, "enclave_pid": enclave_pid,
        "pre_stop_finalized_height": height, "node_log_start": node_log.start_offset(),
        "stopped_finalized_height": stopped_height.expect("observed stop checkpoint"),
        "enclave_log_start": enclave_log.start_offset()}));
    state.replacement = Some(RestartIncarnation {
        node_pid,
        enclave_pid,
        node_log,
        enclave_log,
    });
    world.state.pending_dkg_restart = Some(state);
}

/// Startup must restore the pending boundary/material, activate at the planned
/// epoch exactly once, and leave every validator on one live committee state.
#[then("the recovered pending DKG activates once and consensus continues")]
fn pending_dkg_recovers_and_activates(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner address");
    let mut state = world
        .state
        .pending_dkg_restart
        .take()
        .expect("pending DKG checkpoint");
    let replacement = state.replacement.as_mut().expect("replacement processes");
    let expected_pids = (replacement.node_pid, replacement.enclave_pid);
    let expected_epoch = state.checkpoint.artifact.epoch;
    let mut ports = world.validators.committee_ports();
    ports.push(joiner_port);
    let mut next_height = state
        .before
        .height
        .checked_add(1)
        .expect("boundary scan start");
    let mut activation = None;
    let mut promoted_at = None;
    let mut promotion_error = None;
    for _ in 0..60 {
        assert_eq!(
            world
                .localnet
                .live_validator_and_enclave_pids(idx)
                .expect("owned replacement liveness"),
            expected_pids,
            "replacement identity changed during recovery"
        );
        let finalized = world
            .rpc
            .finalized_result(primary)
            .expect("observe finalized boundary head");
        while activation.is_none() && next_height <= finalized {
            if let Some(artifact) = pending_boundary_at(&world.rpc, primary, next_height)
                .expect("decode canonical finalized boundary")
            {
                assert_eq!(
                    artifact, state.checkpoint.artifact,
                    "finalized a different boundary instead of the recovered pending DKG"
                );
                activation = Some(next_height);
                break;
            }
            next_height = next_height.checked_add(1).expect("boundary scan height");
        }
        if let Some(height) = activation {
            let local_height = world
                .rpc
                .finalized_result(joiner_port)
                .expect("replacement finalized height during promotion");
            if local_height >= height {
                assert_eq!(
                    world
                        .rpc
                        .checkpoint_at(joiner_port, height)
                        .expect("replacement boundary checkpoint"),
                    world
                        .rpc
                        .checkpoint_at(primary, height)
                        .expect("canonical boundary checkpoint")
                );
                match state
                    .checkpoint
                    .verify_active(&state.keys_dir, &state.consensus_public_key)
                {
                    Ok(()) => {
                        promoted_at = Some(local_height);
                        break;
                    }
                    Err(error) => promotion_error = Some(error.to_string()),
                }
            }
        }
        sleep(Duration::from_secs(10));
    }
    let activation =
        activation.expect("restarted joiner never finalized its exact pending DKG boundary");
    let promoted_at = promoted_at.unwrap_or_else(|| panic!(
        "matching active material/pending retirement not observed within recovery allowance: {promotion_error:?}"));
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_promoted",
        "slot": idx, "activation_height": activation, "observed_finalized_height": promoted_at,
        "epoch": expected_epoch, "dkg_cycle": state.checkpoint.artifact.dkg_cycle,
        "outcome_hash": alloy_primitives::keccak256(&state.checkpoint.artifact.outcome)}));
    assert!(
        activation >= state.checkpoint.artifact.planned_activation_height,
        "boundary activated before its planned height"
    );
    world
        .rpc
        .wait_finalized_checkpoint(&ports, activation, 60)
        .expect("all five nodes must finalize the recovered boundary");
    let boundary = world
        .rpc
        .checkpoint_at(primary, activation)
        .expect("activation checkpoint");
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, activation)
                .expect("peer activation checkpoint"),
            boundary
        );
        pending_assert_membership(&world.rpc, port, &addr, activation, 2, true, expected_epoch)
            .expect("finalized activation must have exactly the incoming epoch and ACTIVE joiner");
        let active = eth::read_call_at_result(
            &world.rpc.url(port),
            addresses::VS_ADDR,
            &eth::IValidatorSet::activeValidatorCountCall {},
            activation,
        )
        .expect("read active count at exact finalized activation");
        assert_eq!(
            u64::from(active),
            5,
            "finalized activation must have all five validators"
        );
    }
    let node_log = replacement
        .node_log
        .read()
        .expect("read only replacement node log");
    assert!(
        node_log.contains("recovered durable pending DKG boundary snapshot"),
        "replacement did not recover the installed pending snapshot"
    );
    assert!(
        node_log
            .contains("restored future DKG handoff; current-epoch channels will be acquired first"),
        "replacement missed pre-activation recovery and only restored an already active boundary"
    );
    assert!(
        !node_log.contains("running DKG ceremony"),
        "replacement ran fresh genesis bootstrap instead of recovering completed pending DKG"
    );
    let enclave_log = replacement
        .enclave_log
        .read()
        .expect("read only replacement enclave log");
    assert!(
        enclave_log.contains("unsealed offer key + group signature"),
        "replacement enclave did not unseal the same sealed state"
    );
    // Promotion/retirement was observed before waiting for slow peers. A later
    // legitimate DKG cycle may now write its own pending files.
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("fresh post-activation finality anchor");
    let after = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("five nodes must make new exact finalized hash/root progress after activation");
    // Later legitimate rotations do not invalidate this exact pinned activation.
    // What must never recur is this SAME completed ceremony's boundary.
    for height in activation.checked_add(1).expect("post-activation height")..=after.height {
        if let Some(artifact) = pending_boundary_at(&world.rpc, primary, height)
            .expect("observe post-recovery finalized boundaries")
        {
            assert!(
                artifact.epoch != expected_epoch
                    || artifact.dkg_cycle != state.checkpoint.artifact.dkg_cycle,
                "the recovered completed ceremony activated more than once"
            );
        }
    }
    assert_eq!(
        world
            .localnet
            .live_validator_and_enclave_pids(idx)
            .expect("replacement final liveness"),
        expected_pids
    );
    world
        .state
        .restart_observations
        .push(json!({"phase": "pending_activated",
        "slot": idx, "epoch": expected_epoch, "activation_height": activation,
        "activation_hash": boundary.block_hash, "activation_state_root": boundary.state_root,
        "progress_height": after.height, "progress_hash": after.block_hash,
        "progress_state_root": after.state_root, "node_pid": replacement.node_pid,
        "enclave_pid": replacement.enclave_pid, "pending_material_retired": true}));
    world.state.pending_dkg_restart = Some(state);
}

fn pending_boundary_at(
    rpc: &Rpc,
    port: u16,
    height: u64,
) -> Result<Option<outbe_primitives::consensus::DkgBoundaryArtifact>> {
    let checkpoint = rpc.checkpoint_at(port, height)?;
    let block = eth::raw_json_result(
        &rpc.url(port),
        "eth_getBlockByNumber",
        json!([format!("0x{height:x}"), false]),
    )?;
    let hash: alloy_primitives::B256 = serde_json::from_value(
        block
            .get("hash")
            .cloned()
            .ok_or_else(|| eyre!("finalized block omitted hash"))?,
    )?;
    ensure!(
        hash == checkpoint.block_hash,
        "canonical block changed during boundary observation"
    );
    let encoded = block
        .get("extraData")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre!("finalized block omitted extraData"))?;
    let bytes = hex::decode(encoded.trim_start_matches("0x"))?;
    let artifacts = decode_outbe_block_artifacts(&bytes)
        .map_err(|error| eyre!("invalid finalized block artifacts: {error}"))?;
    Ok(match artifacts.consensus_header_artifact {
        Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) => Some(artifact),
        _ => None,
    })
}

/// Catch the first observable freeze of a 4->5 target and immediately restart
/// the joining node plus enclave, before it can persist completed material.
#[when("a joining validator is restarted during its DKG ceremony")]
fn restart_joiner_during_dkg(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = Some(world.rpc.address_of(&key).expect("joiner identity"));
    restart_capture_incarnations(world, idx).expect("arm owned pre-ceremony intervals");
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let mut ceremony_started = false;
    for _ in 0..1_800 {
        let log = world
            .state
            .lifecycle_incarnations
            .get_mut(&idx)
            .expect("joiner owner")
            .node_log
            .read()
            .expect("current joiner log");
        if log.contains("freezing validator set and starting DKG rotation") {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joiner never entered DKG");
    wait_for_dkg_retry_snapshot(&keys, idx, "dkg_player_retry.hex");
    assert!(
        !world
            .state
            .lifecycle_incarnations
            .get_mut(&idx)
            .expect("joiner owner")
            .node_log
            .read()
            .expect("current joiner log")
            .contains("persisted completed DKG state before activation"),
        "DKG completed before the intended in-flight fault"
    );
    restart_pin_before(world, 1, idx).expect("canonical pending pre-fault state");
    restart_joiner_pair(world, idx, true)
        .expect("restart exact in-flight joiner and enclave with preserved keys");
}

/// An interrupted ceremony may retry, but it must never partially activate;
/// the 4-node committee remains live until one finalized DKG outcome activates.
#[then("the old committee stays live and a later DKG activates the joiner once")]
fn interrupted_dkg_retries_without_partial_activation(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    let activation = restart_activation(world, true)
        .expect("one canonical retry admission; old committee until boundary");
    let mut share_promoted = false;
    for _ in 0..30 {
        if world.localnet.has_share_file(idx) {
            share_promoted = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    assert!(
        share_promoted,
        "retried DKG did not persist its active share"
    );
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "ACTIVE joiner has no current private threshold share"
    );
    restart_check_logs(world, idx, true, false).expect("current enclave recovered sealed state");
    // Drain pre-eligibility accounting (including its permitted boundary miss),
    // then close the ENTIRE five-block eligible signing window.
    let after =
        restart_prove_signing(world, &addr, 60, 60).expect("recovered joiner signing window");
    assert!(
        after.height > activation.height,
        "no post-activation finality"
    );
    restart_check_logs(world, idx, true, false)
        .expect("same recovered incarnation remains healthy");
}

/// Restart at the earliest durable join checkpoint: registration, P2P identity
/// and enclave join are committed, but no stake/readiness or DKG side effect is.
#[when("a registered joining node and enclave restart before staking")]
fn restart_registered_joiner_before_staking(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    let keys = world.localnet.keys_dir(idx);
    world
        .localnet
        .launch_caught_up_joiner(idx, &["--consensus.keys-dir", &keys])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = Some(world.rpc.address_of(&key).expect("joiner identity"));
    restart_capture_incarnations(world, idx).expect("capture registered identity and owners");
    restart_pin_before(world, 0, idx).expect("canonical zero-stake REGISTERED checkpoint");
    restart_joiner_pair(world, idx, false).expect("restart exact registered node and enclave");
}

/// The restart must preserve exactly the registered pre-state; only subsequent
/// stake/readiness may create one pending target and one activation.
#[then("registration survives and the join can activate once")]
fn registered_restart_then_join_activates(world: &mut World) {
    let idx = world.validators.joiner_index();
    let addr = world.state.joiner_addr.clone().expect("joiner identity");
    let checkpoint = restart_fresh_checkpoint(world, 40)
        .expect("registered restart makes all-five fresh finality");
    let state = restart_snapshot(world, checkpoint, &addr, "registered_restart_before_stake")
        .expect("pinned registered restart state");
    restart_require_membership(world, &state, 0)
        .expect("restart preserves registration and zero stake");
    restart_check_logs(world, idx, true, false)
        .expect("registered enclave recovered its sealed identity");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let stake = world.rpc.stake(&key, 1000).expect("stake after restart");
    let staked = restart_finalize_hash(world, &stake).expect("finalized stake on all five nodes");
    let state = restart_snapshot(world, staked, &addr, "registered_restart_staked")
        .expect("pinned pending stake");
    restart_require_membership(world, &state, 1).expect("stake alone creates pending membership");
    // Anchor the admission to the actual stake, not an epoch preceding a
    // legitimate rotation while the registered process was restarting.
    world.state.lifecycle_before = Some(staked);
    let ready = world
        .rpc
        .confirm_ready(&key)
        .expect("confirm after restart");
    restart_finalize_hash(world, &ready).expect("finalized readiness receipt");
    restart_activation(world, false).expect("one canonical post-stake admission");
    assert!(
        world.localnet.has_share_file(idx),
        "activated registered restart has no persisted share"
    );
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(idx)),
        Some(true),
        "activated registered restart has no private signing share"
    );
    restart_prove_signing(world, &addr, 60, 60)
        .expect("registered restart closes an eligible signing window");
    restart_check_logs(world, idx, true, false).expect("registered replacement remains healthy");
}

/// Interrupt one existing committee member only after the scheduled 4->5
/// reshare has actually frozen and entered DKG. The restart must not create a
/// second target or permit a partial activation.
#[when("an active validator and enclave restart during a joining reshare")]
fn restart_active_validator_during_reshare(world: &mut World) {
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_caught_up_joiner(idx, &[])
        .expect("launch joiner");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = Some(world.rpc.address_of(&key).expect("joiner identity"));
    restart_capture_incarnations(world, 3)
        .expect("arm incumbent and survivor identities before reshare");
    world.rpc.stake(&key, 1000).expect("stake joiner");
    world.rpc.confirm_ready(&key).expect("confirm joiner ready");
    let mut ceremony_started = false;
    for _ in 0..1_800 {
        let log = world
            .state
            .lifecycle_incarnations
            .get_mut(&0)
            .expect("primary owner")
            .node_log
            .read()
            .expect("current primary log");
        if log.contains("freezing validator set and starting DKG rotation") {
            ceremony_started = true;
            break;
        }
        sleep(Duration::from_millis(100));
    }
    assert!(ceremony_started, "joining reshare never entered DKG");
    assert!(
        !world
            .state
            .lifecycle_incarnations
            .get_mut(&0)
            .expect("primary owner")
            .node_log
            .read()
            .expect("current primary log")
            .contains("persisted completed DKG state before activation"),
        "reshare completed before the intended active-validator fault"
    );
    let keys = world.localnet.scenario_dir().join("validator-3/data/keys");
    wait_for_dkg_retry_snapshot(&keys, 3, "dkg_dealer_retry.hex");
    wait_for_dkg_retry_snapshot(&keys, 3, "dkg_player_retry.hex");
    restart_pin_before(world, 1, 3)
        .expect("canonical pending frozen target before incumbent fault");
    let mut old = world
        .state
        .lifecycle_incarnations
        .remove(&3)
        .expect("owned incumbent");
    let previous = (old.node_pid, old.enclave_pid);
    let expected_round = restart_inflight_round(
        &old.node_log.read().expect("current incumbent log"),
    )
    .expect("incumbent must still be in the incomplete frozen ceremony immediately before fault");
    world.state.restart_observations.push(json!({
        "phase": "restart_inflight_armed", "index": 3, "node_pid": old.node_pid, "round": expected_round,
    }));
    let dir = world.localnet.scenario_dir().join("validator-3");
    let mut logs = None;
    let mut round = None;
    let observations = &mut world.state.restart_observations;
    world
        .localnet
        .restart_validator_and_enclave_owned_observed(3, previous.0, previous.1, |_| {
            old.node_log.seal()?;
            old.enclave_log.seal()?;
            observations.push(restart_sealed_node_evidence(3, &mut old)?);
            let sealed_round = restart_inflight_round(&old.node_log.read()?)?;
            ensure!(
                sealed_round == expected_round,
                "incumbent frozen target changed during fault/reap"
            );
            round = Some(sealed_round);
            logs = Some((
                LaunchLog::arm(&dir.join("node.log"))?,
                LaunchLog::arm(&dir.join("enclave.log"))?,
            ));
            Ok(())
        })
        .expect("restart exactly the owned incumbent and enclave");
    restart_retain_frozen_target(
        world,
        3,
        round.expect("sealed interrupted incumbent target"),
    )
    .expect("historical pre-fault frozen commitment on all survivors");
    let (node_log, enclave_log) = logs.expect("replacement intervals armed after reaping");
    restart_install_replacement(world, 3, previous, node_log, enclave_log, true)
        .expect("same incumbent public identity; other four owners unchanged");
}

/// The interrupted scheduled target may retry, but activation remains atomic:
/// the joiner enters once, the restarted incumbent remains active, and every
/// node converges on the same epoch and committee.
#[then("the frozen reshare activates once with the restarted validator in lockstep")]
fn active_restart_reshare_converges(world: &mut World) {
    restart_activation(world, true)
        .expect("one canonical frozen-target admission after incumbent recovery");
    restart_check_logs(world, 3, true, true)
        .expect("current incumbent recovered enclave and dealer transcript");
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(3)),
        Some(true),
        "restarted ACTIVE incumbent has no current private threshold share"
    );
    let key = world.validators.get(3).evm_key().expect("incumbent key");
    let address = world.rpc.address_of(&key).expect("incumbent identity");
    restart_prove_signing(world, &address, 60, 60)
        .expect("incumbent closes eligible signing window with all five peers");
    restart_check_logs(world, 3, true, true).expect("same incumbent incarnation remains healthy");
}

#[cfg(test)]
mod restart_observation_tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn signing_requires_five_eligible_blocks_and_their_delayed_accounting() {
        let start = 100;
        let closed = start + 5 + LATE_FINALIZE_WINDOW_K;
        assert!(restart_signing_comparison(start, closed - 1, 1, 1, 1, 1).is_err());
        assert!(restart_signing_comparison(start, closed, 1, 1, 1, 1).unwrap());
        assert!(restart_signing_comparison(start, closed, 1, 1, 1, 2).is_err());
        assert!(restart_signing_comparison(start, closed, 1, 1, 1, 0).is_err());
        assert!(restart_signing_comparison(u64::MAX, u64::MAX, 1, 1, 0, 0).is_err());
    }

    #[test]
    fn later_rotation_requires_a_new_closed_window_not_a_false_failure() {
        let end = 100 + 5 + LATE_FINALIZE_WINDOW_K;
        assert!(!restart_signing_comparison(100, end, 1, 2, 0, 1).unwrap());
        let drained = end + LATE_FINALIZE_WINDOW_K;
        assert!(restart_signing_comparison(
            drained,
            drained + 5 + LATE_FINALIZE_WINDOW_K,
            2,
            2,
            1,
            1
        )
        .unwrap());
        assert!(restart_signing_comparison(100, end, 2, 1, 1, 1).is_err());
        assert!(restart_remaining_tries(std::time::Instant::now()).is_err());
    }

    #[test]
    fn admission_requires_one_canonical_boundary_without_partial_activation() {
        assert!(!restart_boundary_transition(0, 0, None, false, false, true).unwrap());
        assert!(restart_boundary_transition(0, 0, None, false, true, true).is_err());
        assert!(restart_boundary_transition(0, 1, Some(1), false, true, true).unwrap());
        assert!(restart_boundary_transition(1, 1, Some(1), true, true, true).is_err());
        assert!(restart_boundary_transition(1, 2, Some(2), true, false, true).is_err());
        assert!(restart_boundary_transition(0, 2, Some(2), false, true, true).is_err());
        assert!(!restart_boundary_transition(1, 2, Some(2), true, true, true).unwrap());
    }

    #[test]
    fn registered_admission_allows_an_intervening_rotation_but_frozen_target_does_not() {
        assert!(!restart_boundary_transition(0, 1, Some(1), false, false, false).unwrap());
        assert!(restart_boundary_transition(1, 2, Some(2), false, true, false).unwrap());
        assert!(restart_boundary_transition(0, 1, Some(1), false, false, true).is_err());
    }

    #[test]
    fn membership_cannot_pass_with_missing_or_duplicate_fifth_peer() {
        let expected: Vec<_> = (1..=5)
            .map(alloy_primitives::Address::with_last_byte)
            .collect();
        let mut state = RestartPublicState {
            address: expected[4],
            consensus_public_key: alloy_primitives::Bytes::from(vec![1; 48]),
            p2p_version: 1,
            p2p_encoded: alloy_primitives::Bytes::from(vec![1]),
            epoch: 1,
            status: 2,
            stake: alloy_primitives::U256::from(1),
            active: expected.clone(),
            participants: expected.clone(),
            voter_misses: 0,
            supply: alloy_primitives::U256::from(1),
        };
        restart_validate_membership(&state, 2, &expected).unwrap();
        state.participants.pop();
        assert!(restart_validate_membership(&state, 2, &expected).is_err());
        state.participants = expected.clone();
        state.active[4] = expected[3];
        assert!(restart_validate_membership(&state, 2, &expected).is_err());
        assert!(restart_validate_membership(&state, 2, &expected[..4]).is_err());
        state.status = 0;
        state.active = expected[..4].to_vec();
        state.participants = state.active.clone();
        assert!(restart_validate_membership(&state, 0, &expected[..4]).is_err());
        state.stake = alloy_primitives::U256::ZERO;
        restart_validate_membership(&state, 0, &expected[..4]).unwrap();
    }

    #[test]
    fn node_only_recovery_preserves_the_enclave_incarnation() {
        assert!(restart_replacement_matches((1, 2), (3, 2), false));
        assert!(!restart_replacement_matches((1, 2), (3, 4), false));
        assert!(!restart_replacement_matches((1, 2), (1, 2), false));
        assert!(restart_replacement_matches((1, 2), (3, 4), true));
        assert!(!restart_replacement_matches((1, 2), (3, 2), true));
        assert!(!restart_replacement_matches((1, 2), (1, 4), true));
    }

    #[test]
    fn recovery_markers_must_come_from_the_replacement_not_old_or_future_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("public-markers.log");
        let good =
            "restoring durable DKG dealer transcript\nunsealed offer key + group signature\n";
        std::fs::write(&path, good).unwrap();
        let mut interval = LaunchLog::arm(&path).unwrap();
        let observed = interval.read().unwrap();
        assert!(restart_require_markers(&observed, &observed, true, true).is_err());
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            writer,
            "{good}freezing validator set and starting DKG rotation"
        )
        .unwrap();
        interval.seal().unwrap();
        writeln!(writer, "running DKG ceremony").unwrap();
        let observed = interval.read().unwrap();
        restart_require_markers(&observed, &observed, true, true).unwrap();
        assert!(!observed.contains("running DKG ceremony"));
    }

    #[test]
    fn recovery_logs_reject_genesis_fallback_and_byzantine_evidence() {
        let enclave = "unsealed offer key + group signature";
        let dealer = "restoring durable DKG dealer transcript";
        restart_require_markers(dealer, enclave, true, true).unwrap();
        assert!(restart_require_markers(dealer, "", true, true).is_err());
        assert!(restart_require_markers("", enclave, true, true).is_err());
        assert!(restart_require_markers("running DKG ceremony", enclave, true, false).is_err());
        assert!(
            restart_require_markers("byzantine evidence observed", enclave, true, false).is_err()
        );
    }

    #[test]
    fn sealed_fault_rejects_completion_after_the_preliminary_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        let mut log = LaunchLog::arm(&path).unwrap();
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(writer, "freezing validator set and starting DKG rotation dkg_cycle=7 freeze_height=80 planned_activation_height=180").unwrap();
        restart_inflight_round(&log.read().unwrap()).unwrap();
        // This arrives during the snapshot/stop interval, after the first check.
        writeln!(writer, "persisted completed DKG state before activation").unwrap();
        log.seal().unwrap();
        assert!(restart_inflight_round(&log.read().unwrap()).is_err());

        let mut replacement = LaunchLog::arm(&path).unwrap();
        writeln!(writer, "freezing validator set and starting DKG rotation dkg_cycle=8 freeze_height=200 planned_activation_height=300").unwrap();
        replacement.seal().unwrap();
        writeln!(writer, "persisted completed DKG state before activation").unwrap();
        // Completion from a later process cannot alter the sealed verdict.
        assert_eq!(
            restart_inflight_round(&replacement.read().unwrap())
                .unwrap()
                .cycle,
            8
        );
    }

    #[test]
    fn frozen_round_requires_complete_unambiguous_current_fields() {
        let line = "freezing validator set and starting DKG rotation dkg_cycle=7 freeze_height=80 planned_activation_height=180";
        let expected = RestartFrozenRound {
            cycle: 7,
            freeze: 80,
            planned: 180,
        };
        assert_eq!(restart_inflight_round(line).unwrap(), expected);
        for bad in [
            String::new(),
            line.replace("dkg_cycle=7", ""),
            line.replace("freeze_height=80", "freeze_height=bad"),
            line.replace(
                "planned_activation_height=180",
                "planned_activation_height=79",
            ),
            format!("{line} dkg_cycle=8"),
            format!("{line}\n{}", line.replace("dkg_cycle=7", "dkg_cycle=8")),
            format!("{line}\nVRF/DKG material activated"),
        ] {
            assert!(restart_inflight_round(&bad).is_err());
        }
    }

    #[test]
    fn recovered_boundary_requires_the_interrupted_round_and_ordered_commitment() {
        let round = RestartFrozenRound {
            cycle: 7,
            freeze: 80,
            planned: 180,
        };
        let members = vec![
            alloy_primitives::Address::with_last_byte(1),
            alloy_primitives::Address::with_last_byte(2),
        ];
        let commitment = restart_target_commitment(&round, &members);
        let encoded = hex::decode(concat!(
            "0000000000000050",
            "00000000000000b4",
            "0000000000000002",
            "0000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000002",
        ))
        .unwrap();
        assert_eq!(commitment, alloy_primitives::keccak256(encoded));
        let target = RestartFrozenTarget {
            round: round.clone(),
            members: members.clone(),
            commitment,
        };
        restart_validate_frozen_target(&target, &round, commitment, &members, 193).unwrap();
        for field in 0..3 {
            let mut wrong = round.clone();
            match field {
                0 => wrong.cycle += 1,
                1 => wrong.freeze += 1,
                _ => wrong.planned += 1,
            }
            assert!(
                restart_validate_frozen_target(&target, &wrong, commitment, &members, 193).is_err()
            );
        }
        assert!(restart_validate_frozen_target(
            &target,
            &round,
            alloy_primitives::B256::ZERO,
            &members,
            193
        )
        .is_err());
        assert!(restart_validate_frozen_target(
            &target,
            &round,
            commitment,
            &[members[1], members[0]],
            193
        )
        .is_err());
        assert!(
            restart_validate_frozen_target(&target, &round, commitment, &members, 179).is_err()
        );
    }

    #[test]
    fn registered_restart_preserves_bls_and_p2p_identity_across_later_rotations() {
        let before = RestartPublicState {
            address: alloy_primitives::Address::with_last_byte(4),
            consensus_public_key: alloy_primitives::Bytes::from(vec![1; 48]),
            p2p_version: 1,
            p2p_encoded: alloy_primitives::Bytes::from(vec![0, 4, 127, 0, 0, 1, 1, 2]),
            epoch: 1,
            status: 0,
            stake: alloy_primitives::U256::ZERO,
            active: Vec::new(),
            participants: Vec::new(),
            voter_misses: 0,
            supply: alloy_primitives::U256::ONE,
        };
        let mut after = before.clone();
        after.epoch += 1;
        restart_same_registered_identity(&before, &after).unwrap();
        for field in 0..4 {
            let mut wrong = after.clone();
            match field {
                0 => wrong.address = alloy_primitives::Address::with_last_byte(5),
                1 => wrong.consensus_public_key = alloy_primitives::Bytes::from(vec![2; 48]),
                2 => wrong.p2p_version = 2,
                _ => wrong.p2p_encoded = alloy_primitives::Bytes::new(),
            }
            assert!(restart_same_registered_identity(&before, &wrong).is_err());
        }
    }
}
