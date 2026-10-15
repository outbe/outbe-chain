//! Case 1: finalized admission, same-process activation, exit and exact claims.
//! Every observation names the complete owned cohort; a demoted node remains
//! an expected peer. Historical boundary/receipt heights are not "latest" state.

use std::collections::BTreeSet;
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use cucumber::{then, when};
use eyre::{ensure, eyre, Result};
#[cfg(test)]
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use serde::Serialize;
use serde_json::{json, Value};

use crate::internal::{addresses, eth, launch_log::LaunchLog};
use crate::world::rpc::{FinalizedCheckpoint, Rpc, TxOutcome};
use crate::world::state::RestartIncarnation;
use crate::world::World;

mod boundary;
mod rejection;
#[cfg(test)]
mod tests;

use boundary::{boundary, signing_counts, signing_heights};
use rejection::{finalized_deactivation_rejection, DeactivationRejection};

const DEMOTION: &str =
    "no threshold share for this epoch - running consensus engine in VERIFIER mode";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Account {
    address: Address,
    consensus_public_key: Bytes,
    status: u8,
    has_share: bool,
    participant: bool,
    stake: U256,
    mirrored_stake: U256,
    total_staked: U256,
    staking_balance: U256,
    balance: U256,
    unbonding_end: u64,
    voter_misses: u64,
    p2p_version: u8,
    p2p_encoded: Bytes,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Observation {
    supply: U256,
    active: Vec<Address>,
    consensus: Vec<Address>,
    epoch: U256,
    account: Option<Account>,
}

fn checkpoint_json(checkpoint: FinalizedCheckpoint) -> Value {
    json!({
        "height": checkpoint.height, "block_hash": checkpoint.block_hash,
        "state_root": checkpoint.state_root,
    })
}

fn retain(world: &mut World, phase: &str, evidence: Value) {
    world.state.restart_observations.push(json!({
        "phase": phase, "evidence": evidence,
    }));
}

fn address(world: &World, index: usize) -> Result<Address> {
    let key = world.validators.get(index).evm_key()?;
    Ok(world
        .rpc
        .address_of(&key)
        .ok_or_else(|| eyre!("derive lifecycle address"))?
        .parse()?)
}

fn members(world: &World, count: usize) -> Result<Vec<Address>> {
    (0..count).map(|index| address(world, index)).collect()
}

fn exact_members(actual: &[Address], expected: &[Address]) -> Result<()> {
    let actual_set = actual.iter().copied().collect::<BTreeSet<_>>();
    let expected_set = expected.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        !expected.is_empty()
            && actual_set.len() == actual.len()
            && expected_set.len() == expected.len()
            && actual_set == expected_set,
        "lifecycle committee identity mismatch"
    );
    Ok(())
}

/// Check membership before issuing any RPC. Never derive peers from responders.
fn ports(world: &World) -> Result<Vec<u16>> {
    ensure!(
        world.validators.joiner_index() == 4,
        "case 1 requires four founders"
    );
    let slots = world
        .state
        .lifecycle_incarnations
        .keys()
        .copied()
        .collect::<Vec<_>>();
    ensure!(
        slots == vec![0, 1, 2, 3] || slots == vec![0, 1, 2, 3, 4],
        "incomplete lifecycle process cohort"
    );
    Ok(slots
        .into_iter()
        .map(|index| world.validators.http_port(index))
        .collect())
}

/// Admission makes slot 4 mandatory even while it is a non-voting FullNode.
fn joined(world: &mut World) -> Result<()> {
    ensure!(
        world
            .state
            .lifecycle_incarnations
            .keys()
            .copied()
            .collect::<Vec<_>>()
            == vec![0, 1, 2, 3, 4],
        "lifecycle phase omitted an expected owned peer"
    );
    live(world)
}

fn live(world: &mut World) -> Result<()> {
    let count = ports(world)?.len();
    let full_node = count == 5 && world.state.promoted_validator_pid.is_none();
    let expected_validators = (0..if full_node { 4 } else { count }).collect::<Vec<_>>();
    ensure!(
        world.localnet.owned_validator_indices() == expected_validators,
        "lifecycle validator ownership changed"
    );
    for index in 0..count {
        let observed = if full_node && index == 4 {
            let (pid, exit) = world.localnet.owned_full_node_process(index)?;
            ensure!(exit.is_none(), "owned FullNode exited");
            (pid, world.localnet.live_enclave_pid(index)?)
        } else {
            world.localnet.live_validator_and_enclave_pids(index)?
        };
        let expected = world
            .state
            .lifecycle_incarnations
            .get(&index)
            .ok_or_else(|| eyre!("missing lifecycle incarnation"))?;
        ensure!(
            observed == (expected.node_pid, expected.enclave_pid),
            "lifecycle node/enclave incarnation changed for slot {index}"
        );
    }
    Ok(())
}

fn log_bounds(world: &mut World) -> Result<Value> {
    let mut bounds = Vec::new();
    for (index, incarnation) in &mut world.state.lifecycle_incarnations {
        let node_bytes = incarnation.node_log.read()?.len();
        let enclave_bytes = incarnation.enclave_log.read()?.len();
        bounds.push(json!({
            "index": index, "node_pid": incarnation.node_pid,
            "enclave_pid": incarnation.enclave_pid,
            "node_start": incarnation.node_log.start_offset(),
            "node_observed_bytes": node_bytes,
            "enclave_start": incarnation.enclave_log.start_offset(),
            "enclave_observed_bytes": enclave_bytes,
        }));
    }
    Ok(json!(bounds))
}

fn balance(rpc: &Rpc, port: u16, owner: Address, height: u64) -> Result<U256> {
    Ok(serde_json::from_value(eth::raw_json_result(
        &rpc.url(port),
        "eth_getBalance",
        json!([owner, format!("0x{height:x}")]),
    )?)?)
}

fn read_at(world: &World, port: u16, checkpoint: FinalizedCheckpoint) -> Result<Observation> {
    ensure!(
        world.rpc.finalized_result(port)? >= checkpoint.height,
        "peer finality regressed"
    );
    ensure!(
        world.rpc.checkpoint_at(port, checkpoint.height)? == checkpoint,
        "checkpoint changed before read"
    );
    let url = world.rpc.url(port);
    let height = checkpoint.height;
    let supply = eth::read_call_at_result(
        &url,
        addresses::TRIBUTE_ADDR,
        &eth::ITribute::totalSupplyCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    let active = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    let consensus = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveConsensusSetCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    let epoch = eth::read_call_at_result(
        &url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getEpochNumberCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    let account = if let Some(addr) = &world.state.joiner_addr {
        let addr: Address = addr.parse()?;
        let record = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::validatorByAddressCall { addr },
            height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(
            record.validatorAddress == addr,
            "lifecycle record owner mismatch"
        );
        let participant = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::isConsensusParticipantCall { addr },
            height,
        )
        .map_err(|error| eyre!(error))?;
        let stake = eth::read_call_at_result(
            &url,
            addresses::STK_ADDR,
            &eth::IStaking::getStakeCall { validator: addr },
            height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(stake == record.stake, "lifecycle stake mirror mismatch");
        let total_staked = eth::read_call_at_result(
            &url,
            addresses::STK_ADDR,
            &eth::IStaking::getTotalStakedCall {},
            height,
        )
        .map_err(|error| eyre!(error))?;
        let voter_misses = eth::read_call_at_result(
            &url,
            addresses::SLASH_ADDR,
            &eth::ISlashIndicator::getVoterMissCountCall { validator: addr },
            height,
        )
        .map_err(|error| eyre!(error))?;
        let p2p = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::getP2pAddressCall {
                validatorAddress: addr,
            },
            height,
        )
        .map_err(|error| eyre!(error))?;
        Some(Account {
            address: addr,
            consensus_public_key: record.consensusPubkey,
            status: record.status,
            has_share: record.hasBLSShare,
            participant,
            stake,
            mirrored_stake: record.stake,
            total_staked,
            staking_balance: balance(&world.rpc, port, addresses::STK_ADDR, height)?,
            balance: balance(&world.rpc, port, addr, height)?,
            unbonding_end: record.unbondingEnd,
            voter_misses,
            p2p_version: p2p.version,
            p2p_encoded: p2p.encoded,
        })
    } else {
        None
    };
    ensure!(
        world.rpc.checkpoint_at(port, height)? == checkpoint,
        "checkpoint changed during read"
    );
    Ok(Observation {
        supply,
        active,
        consensus,
        epoch,
        account,
    })
}

fn unanimous<'a>(
    expected: &[u16],
    observations: &'a [(u16, Observation)],
) -> Result<&'a Observation> {
    ensure!(
        !expected.is_empty()
            && expected.iter().copied().collect::<BTreeSet<_>>().len() == expected.len()
            && observations
                .iter()
                .map(|(port, _)| *port)
                .collect::<Vec<_>>()
                == expected,
        "missing, duplicated or substituted lifecycle peer"
    );
    let first = &observations[0].1;
    ensure!(
        observations.iter().all(|(_, state)| state == first),
        "pinned lifecycle state diverged"
    );
    Ok(first)
}

fn observe(world: &mut World, phase: &str, checkpoint: FinalizedCheckpoint) -> Result<Observation> {
    live(world)?;
    let expected = ports(world)?;
    let observations = expected
        .iter()
        .map(|&port| Ok((port, read_at(world, port, checkpoint)?)))
        .collect::<Result<Vec<_>>>()?;
    let bounds = log_bounds(world)?;
    retain(
        world,
        phase,
        json!({
            "checkpoint": checkpoint_json(checkpoint), "expected_ports": expected,
            "observations": observations, "processes": bounds,
        }),
    );
    live(world)?;
    Ok(unanimous(&expected, &observations)?.clone())
}

fn wait(
    world: &mut World,
    phase: &str,
    height: u64,
    tries: u32,
) -> Result<(FinalizedCheckpoint, Observation)> {
    live(world)?;
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports(world)?, height, tries)?;
    Ok((checkpoint, observe(world, phase, checkpoint)?))
}

fn fresh(
    world: &mut World,
    phase: &str,
    minimum: u64,
    tries: u32,
) -> Result<(FinalizedCheckpoint, Observation)> {
    live(world)?;
    let before = ports(world)?
        .into_iter()
        .map(|port| Ok((port, world.rpc.finalized_result(port)?)))
        .collect::<Result<Vec<_>>>()?;
    let target = world
        .rpc
        .fresh_finality_target(&ports(world)?)?
        .max(minimum);
    for &(_, height) in &before {
        ensure!(
            target
                >= height
                    .checked_add(2)
                    .ok_or_else(|| eyre!("finality baseline overflow"))?,
            "finality regressed while sampling progress target"
        );
    }
    // Capture the public baseline before the all-peer +2 target, never after it.
    retain(
        world,
        &format!("{phase}_target"),
        json!({"target": target, "sample": before}),
    );
    wait(world, phase, target, tries)
}

fn account(observation: &Observation) -> Result<&Account> {
    observation
        .account
        .as_ref()
        .ok_or_else(|| eyre!("missing registered lifecycle account"))
}

fn receipt_identity(outcome: &TxOutcome, success: bool) -> Result<(u64, B256)> {
    ensure!(
        outcome.success == success,
        "unexpected transaction execution outcome"
    );
    ensure!(
        outcome.receipt["status"].as_str() == Some(if success { "0x1" } else { "0x0" }),
        "malformed or contradictory receipt status"
    );
    let tx: B256 = outcome.transaction_hash.parse()?;
    let receipt_tx: B256 = serde_json::from_value(outcome.receipt["transactionHash"].clone())?;
    ensure!(tx == receipt_tx, "receipt transaction identity mismatch");
    if !success {
        ensure!(
            outcome.receipt["logs"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "reverted transaction retained logs"
        );
    }
    Ok((
        outcome
            .block_number()
            .ok_or_else(|| eyre!("missing receipt height"))?,
        outcome.block_hash()?,
    ))
}

fn finalize(
    world: &mut World,
    phase: &str,
    outcome: &TxOutcome,
    success: bool,
    tries: u32,
) -> Result<(FinalizedCheckpoint, Observation)> {
    let (height, hash) = receipt_identity(outcome, success)?;
    retain(
        world,
        &format!("{phase}_receipt"),
        json!({"receipt": outcome.receipt}),
    );
    live(world)?;
    let expected = ports(world)?;
    let checkpoint = if success {
        world.rpc.finalize_outcome(outcome, &expected, tries)?
    } else {
        world
            .rpc
            .wait_finalized_checkpoint(&expected, height, tries)?;
        let checkpoint = world.rpc.checkpoint_at(expected[0], height)?;
        ensure!(
            checkpoint.block_hash == hash,
            "reverted receipt is not canonical"
        );
        checkpoint
    };
    ensure!(
        checkpoint.height == height && checkpoint.block_hash == hash,
        "receipt checkpoint mismatch"
    );
    Ok((checkpoint, observe(world, phase, checkpoint)?))
}

fn finalized_tx(
    world: &mut World,
    phase: &str,
    hash: &str,
    tries: u32,
) -> Result<(FinalizedCheckpoint, Observation)> {
    let receipt = eth::raw_json_result(
        &world.rpc.url(world.validators.primary_port()),
        "eth_getTransactionReceipt",
        json!([hash]),
    )?;
    finalize(
        world,
        phase,
        &TxOutcome {
            transaction_hash: hash.to_owned(),
            success: true,
            receipt,
        },
        true,
        tries,
    )
}

fn historical(world: &mut World, phase: &str, height: u64, tries: u32) -> Result<Observation> {
    live(world)?;
    world
        .rpc
        .wait_finalized_checkpoint(&ports(world)?, height, tries)?;
    let checkpoint = world
        .rpc
        .checkpoint_at(world.validators.primary_port(), height)?;
    observe(world, phase, checkpoint)
}

fn claim_accounting(
    before: &Account,
    after: &Account,
    amount: U256,
    fee: U256,
    status: u8,
) -> Result<()> {
    same_identity(before, after)?;
    ensure!(
        after.address == before.address
            && after.consensus_public_key == before.consensus_public_key
            && after.stake == before.stake
            && after.mirrored_stake == before.mirrored_stake
            && after.total_staked == before.total_staked
            && after.staking_balance
                == before
                    .staking_balance
                    .checked_sub(amount)
                    .ok_or_else(|| eyre!("claim exceeds escrow"))?
            && after.balance
                == before
                    .balance
                    .checked_add(amount)
                    .and_then(|balance| balance.checked_sub(fee))
                    .ok_or_else(|| eyre!("claim balance overflow/underflow"))?
            && after.status == status
            && !after.participant,
        "claim changed identity, stake, escrow or exact fee accounting"
    );
    Ok(())
}

fn unchanged_bond(before: &Account, after: &Account, status: u8) -> Result<()> {
    same_identity(before, after)?;
    ensure!(
        after.address == before.address
            && after.consensus_public_key == before.consensus_public_key
            && after.stake == before.stake
            && after.mirrored_stake == before.mirrored_stake
            && after.total_staked == before.total_staked
            && after.staking_balance == before.staking_balance
            && after.status == status
            && after.participant
            && after.has_share,
        "deactivation changed bonded value, identity or consensus role"
    );
    Ok(())
}

fn same_identity(before: &Account, after: &Account) -> Result<()> {
    ensure!(
        after.address == before.address
            && after.consensus_public_key == before.consensus_public_key
            && after.p2p_version == before.p2p_version
            && after.p2p_encoded == before.p2p_encoded,
        "lifecycle public identity changed"
    );
    Ok(())
}

fn demotion_seen(text: &str, offset: usize) -> Result<bool> {
    Ok(text
        .get(offset..)
        .ok_or_else(|| eyre!("demotion log prefix changed"))?
        .contains(DEMOTION))
}

#[when(expr = "operator {string} submits a tribute offer")]
fn submit_offer(world: &mut World, name: String) {
    (|| -> Result<()> {
        ensure!(
            world.state.lifecycle_incarnations.is_empty(),
            "lifecycle already initialized"
        );
        // The offer is admitted only from an operator L2Registry knows. This is
        // an offer precondition, so it is established before the scenario's
        // before-offer observation, not between that observation and the offer.
        // That position would shift every later height by the governance window.
        let key = world.validators.by_name(&name)?.evm_key()?;
        crate::features::l2_registration::ensure_tribute_offer_operator(world, &key);
        for index in 0..4 {
            let (node_pid, enclave_pid) = world.localnet.live_validator_and_enclave_pids(index)?;
            let dir = world
                .localnet
                .scenario_dir()
                .join(format!("validator-{index}"));
            // Founders predate this scenario phase. These are observation-prefix
            // bounds, not evidence that their startup happened in this interval.
            let node_log = LaunchLog::checkpoint(&dir.join("node.log"))?;
            let enclave_log = LaunchLog::checkpoint(&dir.join("enclave.log"))?;
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
        let (before, state) = wait(world, "lifecycle_before_offer", 0, 1)?;
        exact_members(&state.consensus, &members(world, 4)?)?;
        ensure!(state.supply.is_zero(), "initial Tribute supply is not zero");
        world.state.lifecycle_before = Some(before);
        world.state.tribute_tx_hash = world.rpc.offer_until_supply_hash(
            &key,
            world
                .state
                .wwd
                .as_deref()
                .ok_or_else(|| eyre!("missing worldwide day"))?,
            world.validators.primary_port(),
            "1",
            20,
        );
        ensure!(
            world.state.tribute_tx_hash.is_some(),
            "initial Tribute did not land"
        );
        Ok(())
    })()
    .expect("submit initial lifecycle offer");
}

#[then("the committee processes and projects the offer")]
fn offer_processed_and_projected(world: &mut World) {
    (|| -> Result<()> {
        let tx = world
            .state
            .tribute_tx_hash
            .clone()
            .ok_or_else(|| eyre!("missing Tribute transaction"))?;
        let (_, state) = finalized_tx(world, "lifecycle_initial_offer", &tx, 30)?;
        ensure!(state.supply == U256::ONE, "initial supply must be one");
        let (_, state) = fresh(world, "lifecycle_initial_offer_progress", 0, 30)?;
        ensure!(
            state.supply == U256::ONE,
            "initial offer executed more than once"
        );
        world.projection.wait_for_tribute_projection(&tx, 60)?;
        Ok(())
    })()
    .expect("finalized initial Tribute and storage projection on every founder");
}

#[when("a full node joins and syncs to the committee tip")]
fn full_node_syncs(world: &mut World) {
    (|| -> Result<()> {
        live(world)?;
        let dir = world.localnet.scenario_dir().join("validator-4");
        std::fs::create_dir_all(&dir)?;
        let node_log = LaunchLog::arm(&dir.join("node.log"))?;
        let enclave_log = LaunchLog::arm(&dir.join("enclave.log"))?;
        world.localnet.launch_joiner_full_node(4, 0, &[])?;
        let (node_pid, exit) = world.localnet.owned_full_node_process(4)?;
        ensure!(exit.is_none(), "FullNode exited during startup");
        let enclave_pid = world.localnet.live_enclave_pid(4)?;
        world.state.lifecycle_incarnations.insert(
            4,
            RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            },
        );
        joined(world)?;
        // Keep the existing sync budget and minimum height, but prove finality.
        wait(world, "lifecycle_full_node_sync", 20, 40)?;
        Ok(())
    })()
    .expect("owned FullNode reaches shared finalized tip");
}

#[then("the full node matches committee supply and state root and is not a participant")]
fn full_node_parity(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let (_, state) = fresh(world, "lifecycle_full_node_parity", 0, 30)?;
        ensure!(state.supply == U256::ONE, "FullNode supply parity");
        exact_members(&state.active, &members(world, 4)?)?;
        exact_members(&state.consensus, &members(world, 4)?)?;
        ensure!(
            !state.consensus.contains(&address(world, 4)?),
            "FullNode became a participant"
        );
        Ok(())
    })()
    .expect("all five owned peers share fresh finalized hash/root and supply");
}

#[when("the synced full node restarts as a registered shareless validator")]
fn full_node_restarts_as_shareless_validator(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let before_offer_key = world.localnet.node_offer_public(4)?;
        // Parent-owned adapter retains the two actual admission receipts.
        let outcomes = world
            .localnet
            .provision_existing_node_as_joiner_observed(4)?;
        ensure!(
            outcomes.len() == 2,
            "admission must retain registration and P2P outcomes"
        );
        world.state.joiner_addr = Some(format!("{:#x}", address(world, 4)?));
        for (phase, outcome) in ["lifecycle_registration", "lifecycle_p2p"]
            .into_iter()
            .zip(&outcomes)
        {
            finalize(world, phase, outcome, true, 60)?;
        }
        let (before, state) = fresh(world, "lifecycle_admission", 0, 60)?;
        let registered = account(&state)?;
        ensure!(
            registered.status == 0
                && !registered.participant
                && !registered.has_share
                && registered.stake.is_zero()
                && registered.consensus_public_key.len() == 48
                && registered.p2p_version == 1
                && registered.p2p_encoded.as_ref() == hex::decode("00047f00000176c4")?,
            "finalized admission identity/role/P2P mismatch"
        );
        ensure!(
            !world.localnet.has_share_file_result(4)?,
            "share exists before DKG"
        );
        let old = world
            .state
            .lifecycle_incarnations
            .get(&4)
            .ok_or_else(|| eyre!("missing FullNode incarnation"))?;
        let old_pids = (old.node_pid, old.enclave_pid);
        let exit = world.localnet.stop_joiner_full_node_owned(4, old_pids.0)?;
        {
            let old = world
                .state
                .lifecycle_incarnations
                .get_mut(&4)
                .ok_or_else(|| eyre!("missing FullNode logs"))?;
            old.node_log.seal()?;
            old.enclave_log.seal()?;
        }
        let bounds = log_bounds(world)?;
        retain(
            world,
            "lifecycle_full_node_stopped",
            json!({
                "node_pid": old_pids.0, "enclave_pid": old_pids.1,
                "exit": exit.to_string(), "checkpoint": checkpoint_json(before),
                "processes": bounds, "offer_public_key": hex::encode(before_offer_key),
            }),
        );
        let dir = world.localnet.scenario_dir().join("validator-4");
        let node_log = LaunchLog::arm(&dir.join("node.log"))?;
        let enclave_log = LaunchLog::checkpoint(&dir.join("enclave.log"))?;
        world.localnet.launch_joiner(4, &[])?;
        let (node_pid, enclave_pid) = world.localnet.live_validator_and_enclave_pids(4)?;
        ensure!(
            node_pid != old_pids.0 && enclave_pid == old_pids.1,
            "role switch replaced enclave or retained node PID"
        );
        ensure!(
            world.localnet.node_offer_public(4)? == before_offer_key,
            "role switch changed resident offer identity"
        );
        world.state.promoted_validator_pid = Some(node_pid);
        world.state.lifecycle_incarnations.insert(
            4,
            RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            },
        );
        // The replacement has just spawned: wait for its RPC/finality before
        // issuing pinned reads. The following shareless step samples fresh +2
        // from every peer after this readiness barrier.
        let (_, after) = wait(
            world,
            "lifecycle_shareless_restart",
            before
                .height
                .checked_add(3)
                .ok_or_else(|| eyre!("restart height overflow"))?,
            60,
        )?;
        observe(world, "lifecycle_restart_preserved_checkpoint", before)?;
        ensure!(
            account(&after)?.consensus_public_key == registered.consensus_public_key
                && account(&after)?.address == registered.address
                && account(&after)?.p2p_version == registered.p2p_version
                && account(&after)?.p2p_encoded == registered.p2p_encoded,
            "role switch changed registered public identity"
        );
        Ok(())
    })()
    .expect("finalized admission and owned same-datadir role switch");
}

#[then("it keeps finalizing without stake, a share, or consensus participation")]
fn shareless_validator_keeps_finalizing_before_stake(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let (_, state) = fresh(world, "lifecycle_shareless_before_stake", 0, 30)?;
        let registered = account(&state)?;
        ensure!(
            registered.status == 0
                && registered.stake.is_zero()
                && !registered.participant
                && !registered.has_share,
            "REGISTERED verifier acquired stake, share or voting role"
        );
        ensure!(
            !world.localnet.has_share_file_result(4)?,
            "REGISTERED verifier has a share"
        );
        exact_members(&state.consensus, &members(world, 4)?)?;
        Ok(())
    })()
    .expect("all five owned peers finalize while joiner is genuinely shareless");
}

#[when("the shareless validator stakes and confirms readiness")]
fn shareless_validator_stakes_confirms(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let key = world.validators.joiner().evm_key()?;
        let tx = world.rpc.stake(&key, 1000)?;
        let (before, state) = finalized_tx(world, "lifecycle_stake", &tx, 30)?;
        ensure!(
            account(&state)?.status == 1 && !account(&state)?.participant,
            "staked joiner must be PENDING"
        );
        world.state.lifecycle_before = Some(before);
        let tx = world.rpc.confirm_ready(&key)?;
        // Keep the offer in flight with the reshare: do not add a warm-up or
        // wait for ACTIVE here. Its receipt is finalized in the next step.
        retain(
            world,
            "lifecycle_confirm_ready_submitted",
            json!({"transaction_hash": tx}),
        );
        Ok(())
    })()
    .expect("finalized stake precedes readiness and the in-flight offer");
}

fn submit_fixture_offer(world: &mut World, validator: usize, missing: &str) -> Result<String> {
    let key = world.validators.get(validator).evm_key()?;
    // Testnet binds the fixture circuit to L2 57005. Each caller retains its owner.
    let tx = world
        .rpc
        .tribute_offer_for_network_with_params(
            &key,
            crate::internal::l2_fixture::FIXTURE_L2_CHAIN_ID,
            crate::world::rpc::TributeOfferParams {
                wwd: world
                    .state
                    .wwd
                    .as_deref()
                    .ok_or_else(|| eyre!("missing worldwide day"))?,
                amounts: ("100", "0"),
                currency: 840,
            },
        )
        .ok_or_else(|| eyre!("{missing}"))?;
    world.state.tribute_tx_hash = Some(tx.clone());
    Ok(tx)
}

#[then("it activates through DKG in the same process and the in-flight offer lands once")]
fn promoted_with_inflight_offer(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let tx = submit_fixture_offer(world, 1, "in-flight offer was not submitted")?;
        world.projection.wait_for_tribute_projection(&tx, 120)?;
        let confirm = world
            .state
            .restart_observations
            .iter()
            .rev()
            .find(|entry| entry["phase"] == "lifecycle_confirm_ready_submitted")
            .and_then(|entry| entry["evidence"]["transaction_hash"].as_str())
            .ok_or_else(|| eyre!("missing readiness receipt identity"))?
            .to_owned();
        finalized_tx(world, "lifecycle_confirm_ready", &confirm, 60)?;
        let (_, offer) = finalized_tx(world, "lifecycle_inflight_offer", &tx, 60)?;
        ensure!(
            offer.supply == U256::from(2),
            "in-flight offer did not execute exactly once"
        );
        let (activation, active) = boundary(world, true, 70, 10)?;
        ensure!(
            account(&active)?.has_share,
            "ACTIVE validator has no on-chain share"
        );
        let mut loaded = false;
        for _ in 0..30 {
            live(world)?;
            if world.localnet.has_share_file_result(4)? {
                loaded = true;
                break;
            }
            sleep(Duration::from_secs(1));
        }
        ensure!(loaded, "activation did not install the threshold share");
        let (closed_height, end_height) = signing_heights(activation)?;
        let closed = historical(world, "lifecycle_pre_eligibility_closed", closed_height, 60)?;
        let end = historical(world, "lifecycle_eligible_signing_closed", end_height, 60)?;
        exact_members(&closed.consensus, &members(world, 5)?)?;
        exact_members(&end.consensus, &members(world, 5)?)?;
        signing_counts(
            account(&active)?.voter_misses,
            account(&closed)?.voter_misses,
            account(&end)?.voter_misses,
        )?;
        let (_, state) = fresh(world, "lifecycle_active_progress", end_height, 16)?;
        ensure!(
            state.supply == U256::from(2),
            "in-flight offer supply changed"
        );
        exact_members(&state.active, &members(world, 5)?)?;
        world
            .projection
            .wait_for_tribute_projection_on_nodes(&tx, 60, 5)?;
        Ok(())
    })()
    .expect("same-process canonical activation and complete eligible signing window");
}

#[when("the promoted validator deactivates")]
fn promoted_deactivates(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let (_, before) = wait(world, "lifecycle_before_deactivation", 0, 1)?;
        let bonded = account(&before)?;
        ensure!(
            bonded.status == 2 && bonded.participant && !bonded.stake.is_zero(),
            "joiner must be bonded ACTIVE"
        );
        world.state.lifecycle_stake_before_exit = Some(bonded.stake);
        world.state.lifecycle_total_before_exit = Some(bonded.total_staked);
        world.state.lifecycle_staking_balance_before_exit = Some(bonded.staking_balance);
        let key = world.validators.joiner().evm_key()?;
        let other_key = world.validators.get(0).evm_key()?;
        let call = eth::IValidatorSet::deactivateValidatorCall {
            validatorAddress: bonded.address,
        };
        let url = world.rpc.url(world.validators.primary_port());
        let unauthorized: TxOutcome =
            eth::send_call_outcome(&url, addresses::VS_ADDR, &other_key, &call, None)?.into();
        let other = address(world, 0)?;
        let unchanged = finalized_deactivation_rejection(
            world,
            "lifecycle_unauthorized_deactivation",
            &unauthorized,
            other,
            DeactivationRejection::Unauthorized,
        )?;
        unchanged_bond(bonded, account(&unchanged)?, 2)?;
        fresh(world, "lifecycle_rejected_deactivation_progress", 0, 30)?;

        let offset = world
            .state
            .lifecycle_incarnations
            .get_mut(&4)
            .ok_or_else(|| eyre!("missing promoted process"))?
            .node_log
            .read()?
            .len();
        retain(
            world,
            "lifecycle_demotion_armed",
            json!({"relative_offset": offset}),
        );
        let tx = world.rpc.deactivate(&key)?;
        // Submit the repeat while EXITING, before waiting for finalized receipt
        // observations. This keeps the original negative assertion in its phase.
        let repeated: TxOutcome =
            eth::send_call_outcome(&url, addresses::VS_ADDR, &key, &call, None)?.into();
        let (checkpoint, exiting) = finalized_tx(world, "lifecycle_deactivation", &tx, 30)?;
        unchanged_bond(bonded, account(&exiting)?, 3)?;
        exact_members(&exiting.consensus, &members(world, 5)?)?;
        world.state.lifecycle_before = Some(checkpoint);
        let repeated_state = finalized_deactivation_rejection(
            world,
            "lifecycle_repeated_deactivation",
            &repeated,
            bonded.address,
            DeactivationRejection::Repeated,
        )?;
        unchanged_bond(account(&exiting)?, account(&repeated_state)?, 3)?;
        Ok(())
    })()
    .expect("canonical voluntary exit, unauthorized rejection and repeated-exit atomicity");
}

#[then("it exits, the committee reshares down, and the node demotes to a follower")]
fn exits_and_demotes(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        let (exit_height, _) = boundary(world, false, 45, 10)?;
        let minimum = exit_height
            .checked_add(1)
            .ok_or_else(|| eyre!("exit height overflow"))?;
        let (_, drained) = fresh(world, "lifecycle_exclusion_and_drain", minimum, 30)?;
        let after = account(&drained)?;
        let amount = world
            .state
            .lifecycle_stake_before_exit
            .ok_or_else(|| eyre!("missing exit stake"))?;
        let total = world
            .state
            .lifecycle_total_before_exit
            .ok_or_else(|| eyre!("missing total stake"))?
            .checked_sub(amount)
            .ok_or_else(|| eyre!("exit stake exceeds total"))?;
        ensure!(
            after.status == 4
                && !after.participant
                && after.stake.is_zero()
                && after.total_staked == total
                && Some(after.staking_balance) == world.state.lifecycle_staking_balance_before_exit,
            "unbonding drain changed native value or failed exact stake removal"
        );
        exact_members(&drained.consensus, &members(world, 4)?)?;
        let offset = world
            .state
            .restart_observations
            .iter()
            .rev()
            .find(|entry| entry["phase"] == "lifecycle_demotion_armed")
            .and_then(|entry| entry["evidence"]["relative_offset"].as_u64())
            .ok_or_else(|| eyre!("missing phase-scoped demotion offset"))?;
        let incarnation = world
            .state
            .lifecycle_incarnations
            .get_mut(&4)
            .ok_or_else(|| eyre!("missing demoted process"))?;
        ensure!(
            demotion_seen(&incarnation.node_log.read()?, usize::try_from(offset)?)?,
            "current process did not demote after deactivation"
        );
        retain(
            world,
            "lifecycle_demotion_observed",
            json!({
                "relative_offset": offset, "marker": DEMOTION, "exit_height": exit_height,
            }),
        );
        let tx = submit_fixture_offer(world, 2, "post-exit offer was not submitted")?;
        ensure!(
            world.rpc.wait_successful_receipt(&tx, 120),
            "post-exit offer receipt did not succeed"
        );
        let (_, offer) = finalized_tx(world, "lifecycle_post_exit_offer", &tx, 40)?;
        ensure!(
            offer.supply == U256::from(3),
            "post-exit offer supply mismatch"
        );
        let (_, progressed) = fresh(world, "lifecycle_demoted_follower_progress", 0, 40)?;
        ensure!(
            progressed.supply == U256::from(3),
            "post-exit offer executed more than once"
        );
        world
            .projection
            .wait_for_tribute_projection_on_nodes(&tx, 60, 5)?;
        Ok(())
    })()
    .expect(
        "canonical exclusion, exact unbonding, current-process demotion and five-peer execution",
    );
}

#[then("its unbonded stake can be claimed with exact accounting")]
fn claim_with_exact_accounting(world: &mut World) {
    (|| -> Result<()> {
        joined(world)?;
        // Preserve the existing short-unbonding wait. The finalized timestamp,
        // not this sleep, is the authority for maturity.
        sleep(Duration::from_secs(10));
        let (checkpoint, before) = fresh(world, "lifecycle_before_claim", 0, 30)?;
        let before_account = account(&before)?;
        ensure!(
            before_account.status == 4 && before_account.unbonding_end > 0,
            "missing unbonding maturity"
        );
        let block = eth::raw_json_result(
            &world.rpc.url(world.validators.primary_port()),
            "eth_getBlockByNumber",
            json!([format!("0x{:x}", checkpoint.height), false]),
        )?;
        let timestamp = u64::from_str_radix(
            block["timestamp"]
                .as_str()
                .ok_or_else(|| eyre!("claim block omitted timestamp"))?
                .trim_start_matches("0x"),
            16,
        )?;
        let hash: B256 = serde_json::from_value(block["hash"].clone())?;
        ensure!(
            hash == checkpoint.block_hash && timestamp >= before_account.unbonding_end,
            "unbonding is not canonically mature"
        );
        let amount = world
            .state
            .lifecycle_stake_before_exit
            .ok_or_else(|| eyre!("missing exited stake"))?;
        let key = world.validators.joiner().evm_key()?;
        let other_key = world.validators.get(0).evm_key()?;

        let receipt = world.rpc.claim_unbonded(&other_key)?;
        let outcome = claim_outcome(receipt)?;
        let (_, unrelated) =
            finalize(world, "lifecycle_unrelated_empty_claim", &outcome, true, 30)?;
        claim_accounting(
            before_account,
            account(&unrelated)?,
            U256::ZERO,
            U256::ZERO,
            4,
        )?;

        let outcome = claim_outcome(world.rpc.claim_unbonded(&key)?)?;
        let fee = outcome
            .gas_cost()
            .ok_or_else(|| eyre!("missing claim gas accounting"))?;
        let (_, claimed) = finalize(world, "lifecycle_claim", &outcome, true, 30)?;
        claim_accounting(account(&unrelated)?, account(&claimed)?, amount, fee, 5)?;

        let repeat = claim_outcome(world.rpc.claim_unbonded(&key)?)?;
        let fee = repeat
            .gas_cost()
            .ok_or_else(|| eyre!("missing repeat claim gas accounting"))?;
        let (_, repeated) = finalize(world, "lifecycle_repeated_claim", &repeat, true, 30)?;
        claim_accounting(account(&claimed)?, account(&repeated)?, U256::ZERO, fee, 5)?;
        let (_, final_state) = fresh(world, "lifecycle_claim_complete", 0, 30)?;
        claim_accounting(
            account(&repeated)?,
            account(&final_state)?,
            U256::ZERO,
            U256::ZERO,
            5,
        )?;
        ensure!(
            account(&final_state)?.stake.is_zero()
                && account(&final_state)?.total_staked
                    == world
                        .state
                        .lifecycle_total_before_exit
                        .ok_or_else(|| eyre!("missing total stake"))?
                        .checked_sub(amount)
                        .ok_or_else(|| eyre!("stake underflow"))?
                && final_state.supply == U256::from(3),
            "final lifecycle accounting or supply mismatch"
        );
        exact_members(&final_state.consensus, &members(world, 4)?)?;
        Ok(())
    })()
    .expect(
        "mature canonical claims, exact fees, idempotency and all five owned peers progressing",
    );
}

fn claim_outcome(receipt: Value) -> Result<TxOutcome> {
    let transaction_hash = receipt["transactionHash"]
        .as_str()
        .ok_or_else(|| eyre!("claim receipt omitted transaction hash"))?
        .to_owned();
    Ok(TxOutcome {
        transaction_hash,
        success: true,
        receipt,
    })
}
