//! DKG failure steps used by `features/validator_lifecycle.feature`.
//! The DKG-failure feature. Freeze a 4->5 reshare target, then take the
//! joiner AND one committee validator offline so the ceremony begins with only
//! 3 online players (< player_threshold) and cannot complete. The OLD committee
//! keeps finalizing on its 3-of-4 quorum (no hard-halt); restoring the downed
//! validator lets a later retry complete and the set reaches 5.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256};
use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use outbe_primitives::consensus::DkgBoundaryArtifact;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::features::common::boot_localnet;
use crate::internal::{addresses, eth, launch_log::LaunchLog};
use crate::world::rpc::{FinalizedCheckpoint, TxOutcome};
use crate::world::state::RestartIncarnation;
use crate::world::World;

mod expiry;

const FOLLOWER_SLOT: usize = 14;

/// Public facts only: never retain the boundary's encoded DKG output in evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BoundaryWitness {
    pub height: u64,
    pub block_hash: B256,
    pub state_root: B256,
    pub epoch: u64,
    pub cycle: u64,
    pub freeze: u64,
    pub planned: u64,
    pub target_hash: B256,
    pub members: Vec<Address>,
}

pub(super) fn dkg_addresses(world: &World, count: usize) -> Result<Vec<Address>> {
    (0..count)
        .map(|index| {
            let key = world.validators.get(index).evm_key()?;
            eth::address_of(&key).ok_or_else(|| eyre!("derive validator-{index} public address"))
        })
        .collect()
}

pub(super) fn capture_dkg_owner(world: &mut World, index: usize) -> Result<()> {
    let (node_pid, enclave_pid) = if index == FOLLOWER_SLOT {
        (
            world.localnet.live_follower_pid("follower")?,
            world.localnet.live_enclave_pid(index)?,
        )
    } else {
        world.localnet.live_validator_and_enclave_pids(index)?
    };
    let dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{index}"));
    world.state.lifecycle_incarnations.insert(
        index,
        RestartIncarnation {
            node_pid,
            enclave_pid,
            node_log: LaunchLog::checkpoint(&dir.join("node.log"))?,
            enclave_log: LaunchLog::checkpoint(&dir.join("enclave.log"))?,
        },
    );
    world.state.restart_observations.push(json!({
        "phase": "dkg_owned_incarnation", "validator": index, "node_pid": node_pid,
        "enclave_pid": enclave_pid,
    }));
    Ok(())
}

/// The caller supplies the fault-model cohort. Never discover it from responsive RPCs.
pub(super) fn dkg_ports(world: &mut World, indices: &[usize]) -> Result<Vec<u16>> {
    validate_dkg_owners(indices, &world.localnet.owned_validator_indices())?;
    let mut expected = indices.to_vec();
    if world
        .state
        .lifecycle_incarnations
        .contains_key(&FOLLOWER_SLOT)
    {
        expected.push(FOLLOWER_SLOT);
    }
    let mut ports = Vec::new();
    for index in expected {
        let actual = if index == FOLLOWER_SLOT {
            (
                world.localnet.live_follower_pid("follower")?,
                world.localnet.live_enclave_pid(index)?,
            )
        } else {
            world.localnet.live_validator_and_enclave_pids(index)?
        };
        let owner = world
            .state
            .lifecycle_incarnations
            .get(&index)
            .ok_or_else(|| eyre!("missing DKG owner {index}"))?;
        ensure!(
            actual == (owner.node_pid, owner.enclave_pid),
            "DKG owner {index} changed incarnation"
        );
        ports.push(world.validators.http_port(index));
    }
    for (&index, owner) in &world.state.lifecycle_incarnations {
        if index != FOLLOWER_SLOT && !indices.contains(&index) {
            ensure!(
                world.localnet.live_enclave_pid(index)? == owner.enclave_pid,
                "deliberately offline node's enclave changed incarnation"
            );
        }
    }
    ensure!(
        !ports.is_empty()
            && ports
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == ports.len(),
        "DKG proof requires distinct expected peers"
    );
    Ok(ports)
}

fn validate_dkg_owners(expected: &[usize], actual: &[usize]) -> Result<()> {
    ensure!(
        !expected.is_empty()
            && expected.windows(2).all(|pair| pair[0] < pair[1])
            && expected == actual,
        "owned validator cohort differs from the intended DKG fault model"
    );
    Ok(())
}

pub(super) fn dkg_membership_at(
    world: &World,
    ports: &[u16],
    checkpoint: FinalizedCheckpoint,
    expected: &[Address],
    joiner: Address,
    status: u8,
) -> Result<()> {
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    ensure!(
        expected.windows(2).all(|pair| pair[0] != pair[1]),
        "duplicate DKG member"
    );
    for &port in ports {
        ensure!(
            world.rpc.finalized_result(port)? >= checkpoint.height,
            "DKG state is not finalized"
        );
        ensure!(
            world.rpc.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "DKG checkpoint disagreement"
        );
        let url = world.rpc.url(port);
        let mut active = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::getActiveValidatorsCall {},
            checkpoint.height,
        )
        .map_err(|e| eyre!(e))?;
        let mut participants = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::getActiveConsensusSetCall {},
            checkpoint.height,
        )
        .map_err(|e| eyre!(e))?;
        let record = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::validatorByAddressCall { addr: joiner },
            checkpoint.height,
        )
        .map_err(|e| eyre!(e))?;
        active.sort_unstable();
        participants.sort_unstable();
        ensure!(
            active == expected && participants == expected,
            "wrong finalized DKG membership"
        );
        ensure!(
            record.validatorAddress == joiner && record.status == status,
            "wrong finalized joiner identity/status"
        );
        ensure!(
            world.rpc.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "DKG state checkpoint changed"
        );
    }
    Ok(())
}

pub(super) fn dkg_boundary_at(
    world: &World,
    ports: &[u16],
    height: u64,
) -> Result<Option<BoundaryWitness>> {
    ensure!(!ports.is_empty(), "boundary proof has no expected peers");
    let checkpoint = world.rpc.checkpoint_at(ports[0], height)?;
    let mut agreed: Option<Option<DkgBoundaryArtifact>> = None;
    for &port in ports {
        ensure!(
            world.rpc.finalized_result(port)? >= height,
            "boundary not finalized by expected peer"
        );
        let block = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getBlockByNumber",
            json!([format!("0x{height:x}"), false]),
        )?;
        let hash: B256 = serde_json::from_value(block["hash"].clone())?;
        ensure!(
            hash == checkpoint.block_hash && world.rpc.checkpoint_at(port, height)? == checkpoint,
            "canonical boundary hash/root disagreement"
        );
        let encoded = block["extraData"]
            .as_str()
            .ok_or_else(|| eyre!("block omitted extraData"))?;
        let bytes = hex::decode(
            encoded
                .strip_prefix("0x")
                .ok_or_else(|| eyre!("invalid artifact encoding"))?,
        )?;
        let artifacts = decode_outbe_block_artifacts(&bytes)
            .map_err(|_| eyre!("invalid canonical block artifacts"))?;
        let boundary = match artifacts.consensus_header_artifact {
            Some(ConsensusHeaderArtifact::BoundaryOutcome(value)) => Some(value),
            _ => None,
        };
        if let Some(expected) = &agreed {
            // Compare full canonical outcomes without disclosing their contents on error.
            ensure!(
                &boundary == expected,
                "canonical DKG outcome differs between peers"
            );
        } else {
            agreed = Some(boundary);
        }
    }
    Ok(agreed.flatten().map(|value| BoundaryWitness {
        height,
        block_hash: checkpoint.block_hash,
        state_root: checkpoint.state_root,
        epoch: value.epoch,
        cycle: value.dkg_cycle,
        freeze: value.freeze_height,
        planned: value.planned_activation_height,
        target_hash: value.target_set_hash,
        members: value.reshare.new_active_set,
    }))
}

pub(super) fn dkg_epoch_at(world: &World, port: u16, height: u64) -> Result<u64> {
    let epoch = eth::read_call_at_result(
        &world.rpc.url(port),
        addresses::VS_ADDR,
        &eth::IValidatorSet::getEpochNumberCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    Ok(epoch.try_into()?)
}

pub(super) fn record_dkg_checkpoint(
    world: &mut World,
    phase: &str,
    ports: &[u16],
    point: FinalizedCheckpoint,
) {
    world
        .state
        .restart_observations
        .push(json!({"phase": phase, "ports": ports,
        "height": point.height, "block_hash": point.block_hash, "state_root": point.state_root}));
}

fn dkg_log(world: &mut World, index: usize) -> Result<String> {
    world
        .state
        .lifecycle_incarnations
        .get_mut(&index)
        .ok_or_else(|| eyre!("missing owned DKG log interval"))?
        .node_log
        .read()
}

fn log_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field}=");
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FrozenTarget {
    cycle: u64,
    freeze: u64,
    planned: u64,
}

fn frozen_target(log: &str, earliest_freeze: u64) -> Result<Option<FrozenTarget>> {
    let mut target = None;
    for line in log
        .lines()
        .filter(|line| line.contains("freezing validator set and starting DKG rotation"))
    {
        let number = |key| -> Result<u64> {
            log_field(line, key)
                .ok_or_else(|| eyre!("frozen-target record omitted {key}"))?
                .parse()
                .map_err(|_| eyre!("malformed frozen-target {key}"))
        };
        let current = FrozenTarget {
            cycle: number("dkg_cycle")?,
            freeze: number("freeze_height")?,
            planned: number("planned_activation_height")?,
        };
        ensure!(
            current.freeze <= current.planned,
            "invalid frozen-target schedule"
        );
        if current.freeze < earliest_freeze {
            continue;
        }
        if let Some(previous) = &target {
            ensure!(previous == &current, "frozen target was replaced");
        }
        target = Some(current);
    }
    Ok(target)
}

fn observed_frozen_target(world: &mut World) -> Result<FrozenTarget> {
    let earliest = dkg_ready_height(world)?;
    let mut agreed = None;
    for index in 0..3 {
        let current = frozen_target(&dkg_log(world, index)?, earliest)?
            .ok_or_else(|| eyre!("validator-{index} has no current frozen target"))?;
        if let Some(expected) = &agreed {
            ensure!(expected == &current, "survivors froze different targets");
        }
        agreed = Some(current);
    }
    agreed.ok_or_else(|| eyre!("no frozen target observations"))
}

fn finalize_dkg_receipts(world: &mut World, ports: &[u16]) -> Result<()> {
    for phase in ["dkg_stake", "dkg_ready"] {
        let row = world
            .state
            .restart_observations
            .iter()
            .find(|row| row["phase"] == phase)
            .ok_or_else(|| eyre!("missing {phase} receipt evidence"))?;
        let outcome = TxOutcome {
            transaction_hash: row["transaction_hash"]
                .as_str()
                .ok_or_else(|| eyre!("missing transaction identity"))?
                .to_owned(),
            success: true,
            receipt: row["receipt"].clone(),
        };
        let point = world.rpc.finalize_outcome(&outcome, ports, 1)?;
        record_dkg_checkpoint(world, &format!("{phase}_finalized"), ports, point);
    }
    Ok(())
}

fn dkg_ready_height(world: &World) -> Result<u64> {
    let ready = world
        .state
        .restart_observations
        .iter()
        .find(|row| row["phase"] == "dkg_ready")
        .ok_or_else(|| eyre!("missing ready receipt"))?;
    let encoded = ready["receipt"]["blockNumber"]
        .as_str()
        .ok_or_else(|| eyre!("ready receipt omitted height"))?;
    Ok(u64::from_str_radix(
        encoded
            .strip_prefix("0x")
            .ok_or_else(|| eyre!("invalid receipt height"))?,
        16,
    )?)
}

fn retain_frozen_target(world: &mut World, ports: &[u16]) -> Result<FrozenTarget> {
    let target = observed_frozen_target(world)?;
    let point = world.rpc.checkpoint_at(ports[0], target.freeze)?;
    let members = dkg_addresses(world, 5)?;
    dkg_membership_at(world, ports, point, &members[..4], members[4], 1)?;
    world
        .state
        .restart_observations
        .push(json!({"phase": "dkg_frozen_target", "target": target}));
    Ok(target)
}

fn retained_frozen_target(world: &World) -> Result<FrozenTarget> {
    let row = world
        .state
        .restart_observations
        .iter()
        .find(|row| row["phase"] == "dkg_frozen_target")
        .ok_or_else(|| eyre!("no retained frozen target"))?;
    Ok(serde_json::from_value(row["target"].clone())?)
}

fn dkg_activation_anchor(boundary_commit_height: u64) -> Result<u64> {
    // The boundary artifact is carried by the first block of the new epoch,
    // one block after the activation anchor used by the rotation schedule.
    boundary_commit_height
        .checked_sub(1)
        .ok_or_else(|| eyre!("DKG boundary commit height cannot be zero"))
}

fn validate_dkg_activation(
    boundary: &BoundaryWitness,
    target: &FrozenTarget,
    members: &[Address],
    old_epoch: u64,
    require_delayed: bool,
) -> Result<()> {
    let anchor = dkg_activation_anchor(boundary.height)?;
    let mut actual = boundary.members.clone();
    let mut expected = members.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    ensure!(
        boundary.cycle == target.cycle
            && boundary.freeze == target.freeze
            && boundary.planned == target.planned
            && anchor >= target.planned
            && boundary.target_hash != B256::ZERO
            && boundary.epoch
                == old_epoch
                    .checked_add(1)
                    .ok_or_else(|| eyre!("DKG epoch overflow"))?
            && actual == expected,
        "activation does not match the frozen DKG target"
    );
    ensure!(
        !require_delayed || anchor > target.planned,
        "activation was not delayed beyond its planned anchor"
    );
    Ok(())
}

fn validate_reveal_interval(
    log: &str,
    target: &FrozenTarget,
    boundary_commit_height: u64,
    expected: &str,
) -> Result<()> {
    let mut inside = false;
    let mut revealed = false;
    let cycle = target.cycle.to_string();
    let height = dkg_activation_anchor(boundary_commit_height)?.to_string();
    for line in log.lines() {
        if line.contains("freezing validator set and starting DKG rotation") {
            inside = log_field(line, "dkg_cycle") == Some(cycle.as_str());
            if inside {
                revealed = false;
            }
        }
        if inside
            && line.contains("DKG: a validator's individual share was REVEALED")
            && log_field(line, "revealed_validator") == Some(expected)
        {
            revealed = true;
        }
        if line.contains("VRF/DKG material activated")
            && log_field(line, "dkg_cycle") == Some(cycle.as_str())
        {
            ensure!(
                inside && revealed && log_field(line, "activation_height") == Some(height.as_str()),
                "offline reveal is not bound to the matching DKG activation interval"
            );
            return Ok(());
        }
    }
    Err(eyre!("missing matching reveal/activation interval"))
}

fn recovered_checkpoint_attempts(remaining: Duration) -> Result<u32> {
    // The shared checkpoint helper waits three seconds after an unavailable
    // observation. Do not grant it a fresh budget after each recovery poll.
    let attempts = u32::try_from(remaining.as_secs() / 3)?;
    ensure!(
        attempts > 0,
        "no DKG recovery checkpoint readiness budget remains"
    );
    Ok(attempts)
}

fn wait_recovered_dkg(
    world: &mut World,
    tries: u32,
    require_reveal: bool,
    off_grid: bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(u64::from(tries) * 10);
    let target = retained_frozen_target(world)?;
    let members = dkg_addresses(world, 5)?;
    let ports = dkg_ports(world, &[0, 1, 2, 3])?;
    let old_epoch = dkg_epoch_at(world, ports[0], target.freeze)?;
    let mut next = target
        .freeze
        .checked_add(1)
        .ok_or_else(|| eyre!("DKG scan height overflow"))?;
    let mut activated = None;
    let mut fresh_target = None;
    loop {
        let ports = dkg_ports(world, &[0, 1, 2, 3])?;
        let attempts =
            recovered_checkpoint_attempts(deadline.saturating_duration_since(Instant::now()))?;
        // A newly restored owned process may not serve finalized RPC yet.
        // Keep every expected peer and the helper's final per-port errors;
        // canonical hash/root disagreement still fails immediately.
        let point = world.rpc.wait_finalized_checkpoint(&ports, 0, attempts)?;
        ensure!(
            Instant::now() < deadline,
            "DKG recovery checkpoint became ready after its observation deadline"
        );
        while next <= point.height {
            if let Some(boundary) = dkg_boundary_at(world, &ports, next)? {
                if activated.is_some() {
                    ensure!(
                        boundary.epoch
                            != old_epoch
                                .checked_add(1)
                                .ok_or_else(|| eyre!("epoch overflow"))?,
                        "recovered DKG epoch activated twice"
                    );
                }
                if boundary.cycle == target.cycle {
                    ensure!(activated.is_none(), "frozen DKG target activated twice");
                    validate_dkg_activation(&boundary, &target, &members, old_epoch, off_grid)?;
                    let at = world.rpc.checkpoint_at(ports[0], next)?;
                    dkg_membership_at(world, &ports, at, &members, members[4], 2)?;
                    world
                        .state
                        .restart_observations
                        .push(json!({"phase": "dkg_activation", "boundary": boundary}));
                    activated = Some(next);
                    fresh_target = Some(world.rpc.fresh_finality_target(&ports)?);
                }
            }
            if activated.is_none() {
                let at = world.rpc.checkpoint_at(ports[0], next)?;
                dkg_membership_at(world, &ports, at, &members[..4], members[4], 1)?;
            }
            next = next
                .checked_add(1)
                .ok_or_else(|| eyre!("DKG scan height overflow"))?;
        }
        if fresh_target.is_some_and(|height| point.height >= height) {
            dkg_membership_at(world, &ports, point, &members, members[4], 2)?;
            if require_reveal {
                let expected = world
                    .state
                    .expected_dkg_reveal
                    .clone()
                    .ok_or_else(|| eyre!("missing expected reveal identity"))?;
                validate_reveal_interval(
                    &dkg_log(world, 0)?,
                    &target,
                    activated.ok_or_else(|| eyre!("missing activated boundary"))?,
                    &expected,
                )?;
            }
            dkg_ports(world, &[0, 1, 2, 3])?;
            record_dkg_checkpoint(world, "dkg_recovered_finality", &ports, point);
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "frozen DKG target did not recover with fresh expected-peer finality"
        );
        sleep(Duration::from_secs(10));
    }
}

/// Setup with a WIDE DKG activation grace so the VRF window outlasts the failed
/// reshare and RECOVERY can be demonstrated (s5:17-25).
#[given("a fresh localnet with a wide DKG activation grace")]
fn tuned_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "180".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "100".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "600".to_string()),
            // Keep validator-3 ACTIVE through the real 120-second failed-DKG
            // timeout. The default E2E threshold would jail it first, silently
            // turning the intended 4->5 target into a 4-member replacement
            // target whose three online players can complete DKG without a retry.
            ("TESTNET_DEV_FELONY_THRESHOLD", "179".to_string()),
        ],
    );
}

/// A compact live network for the permanent-loss safety path. The joiner can
/// sync and confirm before height 40; the frozen target then expires shortly
/// after its height-60 activation boundary.
#[given("a fresh localnet with a short DKG activation grace")]
fn short_grace_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "20".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "6".to_string()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "59".to_string()),
        ],
    );
}

/// Stake + confirm a joiner to freeze a 4->5 reshare target (s5:31-35).
#[when("a staked joiner freezes a 4-to-5 reshare target")]
fn freeze_target(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "the frozen-target fault fixture requires four founders"
    );
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_caught_up_joiner(idx, &[])
        .expect("launch joiner");
    for index in 0..=idx {
        capture_dkg_owner(world, index).expect("capture DKG process identity before readiness");
    }
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let stake = world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    let ready = world.rpc.confirm_ready(&key).expect("confirm ready");
    // Retain receipts now; finalize them on survivors after the existing fault,
    // without adding a pre-fault warmup or waiting for the target to freeze.
    for (phase, hash) in [("dkg_stake", stake), ("dkg_ready", ready)] {
        let receipt = eth::raw_json_result(
            &world.rpc.url(world.validators.primary_port()),
            "eth_getTransactionReceipt",
            json!([hash]),
        )
        .expect("retain DKG transaction receipt");
        world
            .state
            .restart_observations
            .push(json!({"phase": phase, "transaction_hash": hash, "receipt": receipt}));
    }
}

/// Take the joiner + validator-3 offline BEFORE the ceremony so it begins with
/// only 3 online players and cannot complete (s5:37-49).
#[when("the reshare loses quorum before it can complete")]
fn lose_quorum(world: &mut World) {
    let idx = world.validators.joiner_index();
    world.state.expected_dkg_reveal = Some(
        world
            .localnet
            .consensus_public_key(idx)
            .expect("derive offline joiner's consensus public key"),
    );
    let ports = dkg_ports(world, &[0, 1, 2, 3, 4]).expect("exact pre-fault DKG owners");
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, 0, 1)
        .expect("pre-fault canonical checkpoint");
    world.state.lifecycle_before = Some(before);
    world.state.marker_height = Some(before.height);
    record_dkg_checkpoint(world, "dkg_before_fault", &ports, before);
    let old = world
        .state
        .lifecycle_incarnations
        .get(&3)
        .expect("owned validator-3")
        .node_pid;
    world.localnet.stop_joiner(idx).expect("stop joiner");
    let status = world
        .localnet
        .kill_validator_owned(3, old)
        .expect("fault and reap exact validator-3");
    world
        .state
        .restart_observations
        .push(json!({"phase": "dkg_owned_fault", "validator": 3,
        "node_pid": old, "exit_status": status.to_string(), "offline_nodes": [3, idx]}));
    for index in [3, idx] {
        world
            .state
            .lifecycle_incarnations
            .get_mut(&index)
            .expect("old DKG incarnation")
            .node_log
            .seal()
            .expect("seal stopped DKG node interval");
    }
    // Enclaves intentionally stay up; their replacement would change the fault.
    for index in [3, idx] {
        assert_eq!(
            world
                .localnet
                .live_enclave_pid(index)
                .expect("offline node's enclave remains live"),
            world.state.lifecycle_incarnations[&index].enclave_pid
        );
    }
}

#[when("the frozen-target joiner and one validator remain offline")]
fn lose_quorum_permanently(world: &mut World) {
    lose_quorum(world);
    // No ceremony completes in this path, so no share is reconstructed and the
    // recovery scenario's expected-reveal allowance must not apply.
    world.state.expected_dkg_reveal = None;
}

/// Prove the bounded safety behavior without treating it as a liveness fix:
/// the old 4-member set remains authoritative and produces blocks, the 5-member
/// target never partially activates, and progress stops only after VRF expiry.
#[then("the old committee finalizes without partial activation until VRF expiry")]
fn old_committee_reaches_expiry_without_partial_activation(world: &mut World) {
    expiry::observe(world).expect("complete owned DKG expiry ceiling proof");
}

#[then("the surviving validators exit with the frozen-target expiry error")]
fn surviving_validators_fail_closed(world: &mut World) {
    expiry::assert_retained(world).expect("retained exact survivor expiry exits");
}

/// The old committee keeps finalizing through the stalled reshare and the join
/// does not activate (s5:50-63).
#[then("the old committee keeps finalizing through the stalled reshare")]
fn old_committee_keeps_finalizing(world: &mut World) {
    let kill_h = world.state.marker_height.expect("kill height");
    let deadline = Instant::now() + Duration::from_secs(300);
    let ports = dkg_ports(world, &[0, 1, 2]).expect("three owned DKG survivors");
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("all-survivor finality anchor")
        .max(kill_h.checked_add(13).expect("stalled-DKG progress height"));
    let members = dkg_addresses(world, 5).expect("fixed DKG membership");
    loop {
        sleep(Duration::from_secs(10));
        let ports = dkg_ports(world, &[0, 1, 2]).expect("unchanged survivor owners");
        let point = world
            .rpc
            .wait_finalized_checkpoint(&ports, 0, 1)
            .expect("survivor canonical checkpoint");
        let log = dkg_log(world, 0).expect("current survivor interval");
        assert!(
            !log.contains("hard halt"),
            "unexpected hard-halt during recoverable DKG failure"
        );
        if point.height >= target
            && log.contains("DKG reshare failed, retrying frozen target on next check")
        {
            finalize_dkg_receipts(world, &ports)
                .expect("stake/readiness receipts finalized by all survivors");
            retain_frozen_target(world, &ports).expect("exact agreed frozen target");
            dkg_membership_at(world, &ports, point, &members[..4], members[4], 1)
                .expect("old set remains authoritative");
            record_dkg_checkpoint(world, "dkg_stalled_finality", &ports, point);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "stalled target did not retry with fresh survivor finality"
        );
    }
}

/// Keep the failed target frozen until the chain has certified blocks after
/// the originally planned activation height. This proves the later boundary
/// cannot be mistaken for the regular epoch grid by a FullNode follower.
#[then("the old committee crosses the planned activation height without partial activation")]
fn old_committee_crosses_planned_activation(world: &mut World) {
    // This step belongs to the explicitly configured off-grid FullNode scenario.
    // Do not probe availability to decide whether its follower is required.
    capture_dkg_owner(world, FOLLOWER_SLOT).expect("required off-grid FullNode owner");
    // This includes reaching the freeze height with one founder offline. Missed
    // proposer views produced 17-32 second block intervals in the SGX run, so
    // 270 seconds expired at planned + 1 despite continuing finality.
    let deadline = Instant::now() + Duration::from_secs(600);
    let ports = dkg_ports(world, &[0, 1, 2]).expect("three survivors and the FullNode");
    let fresh = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("off-grid finality anchor");
    loop {
        let ports = dkg_ports(world, &[0, 1, 2]).expect("off-grid owners remain unchanged");
        let point = world
            .rpc
            .wait_finalized_checkpoint(&ports, 0, 1)
            .expect("off-grid common checkpoint");
        let earliest = dkg_ready_height(world).expect("readiness receipt height");
        let mut required_height = None;
        if let Some(target) =
            frozen_target(&dkg_log(world, 0).expect("current freeze log"), earliest)
                .expect("decode freeze")
        {
            let required = fresh.max(
                target
                    .planned
                    .checked_add(2)
                    .expect("planned progress height"),
            );
            required_height = Some(required);
            if point.height >= required {
                finalize_dkg_receipts(world, &ports).expect("off-grid admission receipts");
                let target = retain_frozen_target(world, &ports).expect("frozen target agreement");
                let members = dkg_addresses(world, 5).expect("expected DKG addresses");
                dkg_membership_at(world, &ports, point, &members[..4], members[4], 1)
                    .expect("no partial off-grid activation");
                world.state.activation_height = Some(target.planned);
                record_dkg_checkpoint(world, "dkg_past_planned_activation", &ports, point);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "old committee did not finalize beyond the planned activation: \
             finalized={}, required={required_height:?}, fresh={fresh}",
            point.height
        );
        sleep(Duration::from_secs(5));
    }
}

/// Relaunch the downed validator (`localnet.restart` re-launches dead nodes),
/// leaving the joiner down (s5:65-71).
#[when("the downed validator is restored")]
fn restore_validator(world: &mut World) {
    dkg_ports(world, &[0, 1, 2]).expect("only the intended founders are down before repair");
    let old = world.state.lifecycle_incarnations[&3].node_pid;
    let enclave = world.state.lifecycle_incarnations[&3].enclave_pid;
    let dir = world.localnet.scenario_dir().join("validator-3");
    let log =
        LaunchLog::arm(&dir.join("node.log")).expect("arm validator-3 replacement before launch");
    world.localnet.restart().expect("restart committee");
    let actual = world
        .localnet
        .live_validator_and_enclave_pids(3)
        .expect("owned restored founder");
    assert_ne!(actual.0, old, "repair did not replace validator-3");
    assert_eq!(
        actual.1, enclave,
        "node-only fault unexpectedly replaced its enclave"
    );
    let owner = world
        .state
        .lifecycle_incarnations
        .get_mut(&3)
        .expect("retained old founder");
    owner.node_pid = actual.0;
    owner.node_log = log;
    dkg_ports(world, &[0, 1, 2, 3]).expect("four founders restored; joiner remains offline");
    world
        .state
        .restart_observations
        .push(json!({"phase": "dkg_owned_repair", "validator": 3,
        "old_node_pid": old, "node_pid": actual.0, "enclave_pid": actual.1, "offline_nodes": [4]}));
}

/// With 4 online acking players again, a later retry completes and the set
/// reaches 5 (s5:72-75).
#[then("the reshare completes and the active set reaches 5")]
fn reshare_completes(world: &mut World) {
    wait_recovered_dkg(world, 40, true, false)
        .expect("frozen target recovers once with exact live-cohort proof");
}

#[then("the delayed reshare activates off-grid and the active set reaches 5")]
fn delayed_reshare_activates_off_grid(world: &mut World) {
    wait_recovered_dkg(world, 60, true, true)
        .expect("actual delayed boundary and fresh FullNode/founder convergence");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_checkpoint_readiness_uses_only_the_remaining_budget() {
        assert_eq!(
            recovered_checkpoint_attempts(Duration::from_secs(3)).unwrap(),
            1
        );
        assert_eq!(
            recovered_checkpoint_attempts(Duration::from_secs(29)).unwrap(),
            9
        );
        assert_eq!(
            recovered_checkpoint_attempts(Duration::from_secs(270)).unwrap(),
            90
        );
        assert!(recovered_checkpoint_attempts(Duration::ZERO).is_err());
        assert!(recovered_checkpoint_attempts(Duration::from_millis(2999)).is_err());
    }

    #[test]
    fn dkg_fault_cohorts_do_not_restore_the_offline_joiner_or_omit_a_survivor() {
        validate_dkg_owners(&[0, 1, 2], &[0, 1, 2]).unwrap();
        validate_dkg_owners(&[0, 1, 2, 3], &[0, 1, 2, 3]).unwrap();
        for actual in [
            vec![0, 1],
            vec![0, 1, 2, 3],
            vec![0, 1, 2, 4],
            vec![0, 1, 2, 2],
        ] {
            assert!(validate_dkg_owners(&[0, 1, 2], &actual).is_err());
        }
        assert!(validate_dkg_owners(&[0, 1, 2, 3], &[0, 1, 2, 3, 4]).is_err());
        assert!(validate_dkg_owners(&[], &[]).is_err());
    }

    #[test]
    fn frozen_target_observation_requires_exact_public_fields() {
        let line = "INFO freezing validator set and starting DKG rotation dkg_cycle=7 current_height=80 freeze_height=80 planned_activation_height=180";
        let expected = FrozenTarget {
            cycle: 7,
            freeze: 80,
            planned: 180,
        };
        assert_eq!(frozen_target(line, 0).unwrap(), Some(expected));
        assert_eq!(frozen_target(line, 81).unwrap(), None);
        assert_eq!(frozen_target("unrelated DKG progress", 0).unwrap(), None);
        for wrong in [
            line.replace("dkg_cycle=7", "dkg_cycle=bad"),
            line.replace("freeze_height=80", ""),
            line.replace(
                "planned_activation_height=180",
                "planned_activation_height=79",
            ),
            format!("{line}\n{}", line.replace("dkg_cycle=7", "dkg_cycle=8")),
        ] {
            assert!(frozen_target(&wrong, 0).is_err());
        }
    }

    #[test]
    fn recovery_binds_actual_boundary_to_the_same_frozen_target() {
        let target = FrozenTarget {
            cycle: 7,
            freeze: 80,
            planned: 180,
        };
        let members: Vec<_> = (1..=5).map(Address::repeat_byte).collect();
        let boundary = BoundaryWitness {
            height: 193,
            block_hash: B256::repeat_byte(2),
            state_root: B256::repeat_byte(3),
            epoch: 4,
            cycle: 7,
            freeze: 80,
            planned: 180,
            target_hash: B256::repeat_byte(1),
            members: members.clone(),
        };
        validate_dkg_activation(&boundary, &target, &members, 3, false).unwrap();
        validate_dkg_activation(&boundary, &target, &members, 3, true).unwrap();
        let mut on_time = boundary.clone();
        on_time.height = 181;
        validate_dkg_activation(&on_time, &target, &members, 3, false).unwrap();
        assert!(validate_dkg_activation(&on_time, &target, &members, 3, true).is_err());
        for height in [0, 179, 180] {
            let mut early = boundary.clone();
            early.height = height;
            assert!(validate_dkg_activation(&early, &target, &members, 3, false).is_err());
        }
        for defect in 0..7 {
            let mut wrong = boundary.clone();
            match defect {
                0 => wrong.cycle += 1,
                1 => wrong.epoch += 1,
                2 => wrong.freeze += 1,
                3 => wrong.planned += 1,
                4 => wrong.height = 179,
                5 => wrong.target_hash = B256::ZERO,
                _ => {
                    wrong.members.pop();
                }
            }
            assert!(validate_dkg_activation(&wrong, &target, &members, 3, false).is_err());
        }
    }

    #[test]
    fn reveal_must_belong_to_the_matching_activation_not_a_later_ceremony() {
        let target = FrozenTarget {
            cycle: 7,
            freeze: 80,
            planned: 180,
        };
        let freeze = "freezing validator set and starting DKG rotation dkg_cycle=7";
        let reveal =
            "DKG: a validator's individual share was REVEALED revealed_validator=public-key";
        let activated = "VRF/DKG material activated dkg_cycle=7 activation_height=193";
        validate_reveal_interval(
            &format!("{freeze}\n{reveal}\n{activated}"),
            &target,
            194,
            "public-key",
        )
        .unwrap();
        // Captured scenario 8: anchor 180 is committed in boundary block 181.
        validate_reveal_interval(
            &format!("{freeze}\n{reveal}\n{}", activated.replace("193", "180")),
            &target,
            181,
            "public-key",
        )
        .unwrap();
        assert!(validate_reveal_interval(
            &format!("{freeze}\n{reveal}\n{activated}"),
            &target,
            0,
            "public-key",
        )
        .is_err());
        for log in [
            format!("{reveal}\n{freeze}\n{activated}"),
            format!("{freeze}\n{activated}\n{reveal}"),
            format!("{freeze}\n{reveal}\n{}", activated.replace("193", "194")),
            format!(
                "{freeze}\n{}\n{activated}",
                reveal.replace("public-key", "foreign-key")
            ),
            format!(
                "{freeze}\n{reveal}\n{}",
                activated.replace("dkg_cycle=7", "dkg_cycle=8")
            ),
            format!(
                "{}\n{reveal}\n{activated}",
                freeze.replace("dkg_cycle=7", "dkg_cycle=8")
            ),
        ] {
            assert!(validate_reveal_interval(&log, &target, 194, "public-key").is_err());
        }
    }
}
