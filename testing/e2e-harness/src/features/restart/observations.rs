use super::committee_checkpoint_json;

use crate::internal::addresses;
use crate::internal::eth;
use crate::internal::launch_log::LaunchLog;

use crate::world::rpc::FinalizedCheckpoint;

use crate::world::rpc::TxOutcome;

use crate::world::state::RestartIncarnation;
use crate::world::World;

use eyre::ensure;
use eyre::eyre;
use eyre::Result;

use serde_json::json;

use std::time::Duration;

// Observation helpers for cases 2/5/6/7 only. Cases 3/4 retain their own proofs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct RestartPublicState {
    pub(super) address: alloy_primitives::Address,
    pub(super) consensus_public_key: alloy_primitives::Bytes,
    pub(super) p2p_version: u8,
    pub(super) p2p_encoded: alloy_primitives::Bytes,
    pub(super) epoch: u64,
    pub(super) status: u8,
    pub(super) stake: alloy_primitives::U256,
    pub(super) active: Vec<alloy_primitives::Address>,
    pub(super) participants: Vec<alloy_primitives::Address>,
    pub(super) voter_misses: u64,
    pub(super) supply: alloy_primitives::U256,
}

pub(super) fn restart_ports(world: &World) -> Result<Vec<u16>> {
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

pub(super) fn restart_assert_live(world: &mut World) -> Result<()> {
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

pub(super) fn restart_capture_incarnations(world: &mut World, restarted: usize) -> Result<()> {
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

pub(super) fn restart_public_state(
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

pub(super) fn restart_same_registered_identity(
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

pub(super) fn restart_snapshot(
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

pub(super) fn restart_require_membership(
    world: &World,
    state: &RestartPublicState,
    status: u8,
) -> Result<()> {
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

pub(super) fn restart_validate_membership(
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

pub(super) fn restart_pin_before(world: &mut World, status: u8, restarted: usize) -> Result<()> {
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

pub(super) fn restart_fresh_checkpoint(
    world: &mut World,
    tries: u32,
) -> Result<FinalizedCheckpoint> {
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

pub(super) fn restart_remaining_tries(deadline: std::time::Instant) -> Result<u32> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    ensure!(
        !remaining.is_zero(),
        "restart observation allowance expired"
    );
    Ok(u32::try_from(remaining.as_secs().div_ceil(3))?.max(1))
}

pub(super) fn restart_finalize_hash(world: &mut World, hash: &str) -> Result<FinalizedCheckpoint> {
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

pub(super) fn restart_replacement_matches(
    previous: (u32, u32),
    current: (u32, u32),
    enclave_replaced: bool,
) -> bool {
    current.0 != previous.0 && (current.1 != previous.1) == enclave_replaced
}

pub(super) fn restart_sealed_node_evidence(
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

pub(super) fn restart_check_logs(
    world: &mut World,
    index: usize,
    unseal: bool,
    dealer: bool,
) -> Result<()> {
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

pub(super) fn restart_require_markers(
    node: &str,
    enclave: &str,
    unseal: bool,
    dealer: bool,
) -> Result<()> {
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
