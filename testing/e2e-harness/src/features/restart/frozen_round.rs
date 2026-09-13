use super::committee_checkpoint_json;
use super::restart_assert_live;
use super::restart_replacement_matches;
use super::restart_sealed_node_evidence;

use crate::internal::addresses;
use crate::internal::eth;
use crate::internal::launch_log::LaunchLog;

use crate::world::state::RestartIncarnation;
use crate::world::World;

use eyre::ensure;
use eyre::eyre;
use eyre::Result;

use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct RestartFrozenRound {
    pub(super) cycle: u64,
    pub(super) freeze: u64,
    pub(super) planned: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct RestartFrozenTarget {
    pub(super) round: RestartFrozenRound,
    // Commonware public-key order, as used by build_boundary_artifact.
    pub(super) members: Vec<alloy_primitives::Address>,
    pub(super) commitment: alloy_primitives::B256,
}

pub(super) fn restart_inflight_round(log: &str) -> Result<RestartFrozenRound> {
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

pub(super) fn restart_target_commitment(
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

pub(super) fn restart_retain_frozen_target(
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

pub(super) fn restart_validate_frozen_target(
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

pub(super) fn restart_install_replacement(
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

pub(super) fn restart_joiner_pair(world: &mut World, index: usize, in_flight: bool) -> Result<()> {
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
