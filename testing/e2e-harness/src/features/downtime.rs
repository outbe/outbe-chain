//! Finalized, owned-process proof of one downtime felony and no repeated burn.

use std::collections::BTreeSet;
use std::os::unix::process::ExitStatusExt;
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use outbe_primitives::storage::types::StorageKey;
use outbe_primitives::units::checked_whole_coen_to_native;
use serde_json::{json, Value};

use crate::internal::{addresses, eth};
use crate::world::rpc::{FinalizedCheckpoint, TxOutcome};
use crate::world::state::{
    DowntimeAccounting, DowntimeFelonyEvent, DowntimeNode, DowntimeObservation, DowntimeState,
};
use crate::world::World;

const TOP_UP_COEN: u64 = 100;

#[given("the slashing config is readable")]
fn slashing_config_readable(world: &mut World) {
    assert!(
        world.rpc.slash_percent().is_some(),
        "slashing config not readable"
    );
}

#[given(expr = "validator {string} starts active")]
fn validator_starts_active(world: &mut World, name: String) {
    prepare_downtime(world, &name).expect("prepare finalized downtime baseline");
}

fn prepare_downtime(world: &mut World, name: &str) -> Result<()> {
    ensure!(world.state.downtime.is_none(), "downtime already prepared");
    let validator = world.validators.by_name(name)?;
    let victim_index = validator.index;
    let key = validator.evm_key()?;
    let victim: Address = world
        .rpc
        .address_of(&key)
        .ok_or_else(|| eyre!("derive downtime victim address"))?
        .parse()?;
    let ports = world.validators.committee_ports();
    ensure!(ports.len() >= 4, "downtime requires a surviving BFT quorum");
    ensure!(
        victim_index < ports.len(),
        "victim is not a committee member"
    );
    let nodes = ports
        .iter()
        .enumerate()
        .map(|(index, &port)| {
            let (node_pid, enclave_pid) = world.localnet.live_validator_and_enclave_pids(index)?;
            Ok(DowntimeNode {
                index,
                port,
                node_pid,
                enclave_pid,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
    let before = observe_at(world, victim, &ports, checkpoint)?;
    retain(
        world,
        "downtime_before_top_up",
        json!({"nodes": nodes, "state": before}),
    );
    let account = agreed_account(&ports, &before.accounts)?;
    ensure!(account.status == 2, "victim must start ACTIVE");
    ensure!(
        account.unbonding_head.is_zero(),
        "victim has queued unbonding"
    );
    ensure!(
        account.stake == account.mirrored_stake,
        "pre-top-up stake mirror mismatch"
    );

    // The typed response supplies production defaults when a config slot is unset.
    // Retain and compare the pinned raw slots throughout, so a changing config
    // cannot silently change the accounting oracle.
    let config = eth::raw_json_result(&world.rpc.url(ports[0]), "outbe_getSlashConfig", json!([]))?;
    let percent = config["slashAmountPercent"]
        .as_u64()
        .ok_or_else(|| eyre!("slash config omitted percent"))?;
    let threshold = config["voterFelonyThreshold"]
        .as_u64()
        .ok_or_else(|| eyre!("slash config omitted voter felony threshold"))?;
    ensure!(
        (1..=100).contains(&percent) && threshold > 0,
        "invalid slash config"
    );
    for (raw, effective) in account.slash_config.iter().zip([percent, threshold]) {
        ensure!(
            raw.is_zero() || *raw == U256::from(effective),
            "slash config changed at baseline"
        );
    }

    let transaction_hash = world.rpc.stake(&key, TOP_UP_COEN)?;
    let receipt = world
        .rpc
        .transaction_receipt(&transaction_hash, ports[0])
        .ok_or_else(|| eyre!("top-up receipt missing"))?;
    let outcome = TxOutcome {
        transaction_hash,
        success: receipt["status"].as_str() == Some("0x1"),
        receipt,
    };
    retain(
        world,
        "downtime_top_up_receipt",
        json!({
            "whole_coen": TOP_UP_COEN, "receipt": outcome.receipt, "config": config,
        }),
    );
    let checkpoint = world.rpc.finalize_outcome(&outcome, &ports, 20)?;
    ensure!(
        checkpoint.height > before.height,
        "top-up did not follow baseline"
    );
    let after = observe_at(world, victim, &ports, checkpoint)?;
    retain(world, "downtime_top_up_finalized", json!({"state": after}));
    validate_top_up(account, agreed_account(&ports, &after.accounts)?)?;
    let state = DowntimeState {
        victim,
        victim_index,
        nodes,
        percent,
        threshold,
        before: after,
        fault_target: None,
        penalty: None,
        event: None,
    };
    check_owned_processes(world, &state, false)?;
    world.state.downtime = Some(state);
    Ok(())
}

#[when(expr = "validator {string} is killed")]
fn validator_is_killed(world: &mut World, name: String) {
    fault_validator(world, &name).expect("fault and reap exact owned validator");
}

fn fault_validator(world: &mut World, name: &str) -> Result<()> {
    let mut state = world
        .state
        .downtime
        .clone()
        .ok_or_else(|| eyre!("downtime baseline missing"))?;
    ensure!(state.fault_target.is_none(), "victim already faulted");
    ensure!(
        world.validators.by_name(name)?.index == state.victim_index,
        "fault target changed"
    );
    check_owned_processes(world, &state, false)?;
    let ports = expected_ports(&state, false);
    let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
    let before_kill = observe_at(world, state.victim, &ports, checkpoint)?;
    retain(
        world,
        "downtime_before_fault",
        json!({"state": before_kill, "nodes": state.nodes}),
    );
    ensure!(
        agreed_account(&ports, &before_kill.accounts)?
            == agreed_account(&ports, &state.before.accounts)?,
        "accounting changed between top-up and fault"
    );
    state.before = before_kill;
    let victim = &state.nodes[state.victim_index];
    let exit = world
        .localnet
        .kill_validator_owned(victim.index, victim.node_pid)?;
    retain(
        world,
        "downtime_owned_fault",
        json!({
            "victim": victim, "exit_code": exit.code(), "exit_signal": exit.signal(),
        }),
    );
    ensure!(
        exit.signal() == Some(9),
        "owned fault did not exit by SIGKILL"
    );
    check_owned_processes(world, &state, true)?;
    state.fault_target = Some(
        world
            .rpc
            .fresh_finality_target(&expected_ports(&state, true))?,
    );
    retain(
        world,
        "downtime_fault_target",
        json!({"height": state.fault_target}),
    );
    world.state.downtime = Some(state);
    Ok(())
}

#[then("the committee keeps finalizing until the validator is slashed exactly once")]
fn committee_keeps_finalizing_until_one_slash(world: &mut World) {
    observe_penalty(world).expect("surviving quorum must finalize exactly one downtime felony");
}

fn observe_penalty(world: &mut World) -> Result<()> {
    let mut state = world
        .state
        .downtime
        .clone()
        .ok_or_else(|| eyre!("downtime baseline missing"))?;
    let target = state
        .fault_target
        .ok_or_else(|| eyre!("owned fault missing"))?;
    let ports = expected_ports(&state, true);
    let all_ports = expected_ports(&state, false);
    let before = agreed_account(&all_ports, &state.before.accounts)?;
    let expected_count = before
        .slash_count
        .checked_add(1)
        .ok_or_else(|| eyre!("slash count overflow"))?;
    // Keep the original 80 x 3-second penalty budget; no nested readiness wait.
    for _ in 0..80 {
        check_owned_processes(world, &state, true)?;
        let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
        let observed = observe_at(world, state.victim, &ports, checkpoint)?;
        retain(world, "downtime_penalty_poll", json!({"state": observed}));
        let account = agreed_account(&ports, &observed.accounts)?;
        ensure!(
            account.slash_count <= expected_count,
            "multiple validator slashes"
        );
        if checkpoint.height >= target && account.slash_count == expected_count {
            validate_penalty(before, account, state.percent)?;
            let event = observe_event(world, &state, &ports, checkpoint.height)?;
            ensure!(
                event.felony_count == account.felony_count,
                "event/counter mismatch"
            );
            check_owned_processes(world, &state, true)?;
            state.penalty = Some(observed);
            state.event = Some(event);
            world.state.downtime = Some(state);
            return Ok(());
        }
        // While awaiting the first slash, no accounting surface may drift.
        if account.slash_count == before.slash_count {
            ensure!(
                account == before,
                "accounting changed without the expected slash"
            );
        }
        sleep(Duration::from_secs(3));
    }
    Err(eyre!(
        "single finalized downtime felony not observed within 80 attempts"
    ))
}

#[then("continued downtime does not slash the validator twice")]
fn continued_downtime_is_idempotent(world: &mut World) {
    observe_idempotence(world).expect("continued downtime must not repeat punitive accounting");
}

fn observe_idempotence(world: &mut World) -> Result<()> {
    let state = world
        .state
        .downtime
        .clone()
        .ok_or_else(|| eyre!("downtime baseline missing"))?;
    let penalty = state
        .penalty
        .as_ref()
        .ok_or_else(|| eyre!("penalty checkpoint missing"))?;
    let event = state
        .event
        .as_ref()
        .ok_or_else(|| eyre!("penalty event missing"))?;
    let ports = expected_ports(&state, true);
    let target = penalty
        .height
        .checked_add(36)
        .ok_or_else(|| eyre!("idempotence height overflow"))?;
    // The previous HEAD > (marker + 35) condition means at least 36 new blocks.
    for _ in 0..40 {
        check_owned_processes(world, &state, true)?;
        let checkpoint = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
        let observed = observe_at(world, state.victim, &ports, checkpoint)?;
        retain(
            world,
            "downtime_idempotence_poll",
            json!({"state": observed, "target": target}),
        );
        ensure!(
            agreed_account(&ports, &observed.accounts)?
                == agreed_account(&ports, &penalty.accounts)?,
            "punitive accounting changed during continued downtime"
        );
        if observed.height >= target {
            validate_idempotence(&ports, penalty, &observed)?;
            let unchanged_event = observe_event(world, &state, &ports, observed.height)?;
            ensure!(
                &unchanged_event == event,
                "felony event changed or repeated"
            );
            check_owned_processes(world, &state, true)?;
            retain(
                world,
                "downtime_idempotence_verified",
                json!({
                    "state": observed, "event": unchanged_event, "nodes": state.nodes,
                }),
            );
            return Ok(());
        }
        sleep(Duration::from_secs(3));
    }
    Err(eyre!(
        "survivors did not finalize 36 further blocks within 40 attempts"
    ))
}

fn expected_ports(state: &DowntimeState, faulted: bool) -> Vec<u16> {
    state
        .nodes
        .iter()
        .filter(|node| !faulted || node.index != state.victim_index)
        .map(|node| node.port)
        .collect()
}

fn check_owned_processes(world: &mut World, state: &DowntimeState, faulted: bool) -> Result<()> {
    for node in &state.nodes {
        if faulted && node.index == state.victim_index {
            ensure!(
                world.localnet.validator_pid(node.index).is_err(),
                "victim node is still owned"
            );
            ensure!(
                world.localnet.live_enclave_pid(node.index)? == node.enclave_pid,
                "victim enclave exited or changed incarnation"
            );
        } else {
            ensure!(
                world.localnet.live_validator_and_enclave_pids(node.index)?
                    == (node.node_pid, node.enclave_pid),
                "expected committee incarnation exited or changed"
            );
        }
    }
    Ok(())
}

fn retain(world: &mut World, kind: &str, observation: Value) {
    world
        .state
        .restart_observations
        .push(json!({"kind": kind, "observation": observation}));
}

fn storage_word(url: &str, address: Address, slot: U256, height: u64) -> Result<U256> {
    Ok(serde_json::from_value(eth::raw_json_result(
        url,
        "eth_getStorageAt",
        json!([address, format!("{slot:#x}"), format!("0x{height:x}")]),
    )?)?)
}

fn observe_at(
    world: &World,
    victim: Address,
    ports: &[u16],
    checkpoint: FinalizedCheckpoint,
) -> Result<DowntimeObservation> {
    let mut accounts = Vec::with_capacity(ports.len());
    for &port in ports {
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
        let record = eth::read_call_at_result(
            &url,
            addresses::VS_ADDR,
            &eth::IValidatorSet::validatorByAddressCall { addr: victim },
            height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(
            record.validatorAddress == victim,
            "validator record owner mismatch"
        );
        let stake = eth::read_call_at_result(
            &url,
            addresses::STK_ADDR,
            &eth::IStaking::getStakeCall { validator: victim },
            height,
        )
        .map_err(|error| eyre!(error))?;
        let total_staked = eth::read_call_at_result(
            &url,
            addresses::STK_ADDR,
            &eth::IStaking::getTotalStakedCall {},
            height,
        )
        .map_err(|error| eyre!(error))?;
        let felony_count = eth::read_call_at_result(
            &url,
            addresses::SLASH_ADDR,
            &eth::ISlashIndicator::getFelonyCountCall { validator: victim },
            height,
        )
        .map_err(|error| eyre!(error))?;
        let staking_balance = serde_json::from_value(eth::raw_json_result(
            &url,
            "eth_getBalance",
            json!([addresses::STK_ADDR, format!("0x{height:x}")]),
        )?)?;
        // Staking slot 9 is the per-validator unbonding head (idx+1; zero is empty).
        // Reuse production's Solidity storage-key derivation, not a second hash implementation.
        let unbonding_head = storage_word(
            &url,
            addresses::STK_ADDR,
            victim.mapping_slot(U256::from(9)),
            height,
        )?;
        let slash_config = [
            storage_word(&url, addresses::SLASH_ADDR, U256::from(3), height)?,
            storage_word(&url, addresses::SLASH_ADDR, U256::from(12), height)?,
        ];
        accounts.push((
            port,
            DowntimeAccounting {
                stake,
                mirrored_stake: record.stake,
                total_staked,
                staking_balance,
                status: record.status,
                slash_count: record.slashCount,
                felony_count,
                unbonding_head,
                slash_config,
            },
        ));
        ensure!(
            world.rpc.checkpoint_at(port, height)? == checkpoint,
            "checkpoint changed during read"
        );
    }
    agreed_account(ports, &accounts)?;
    Ok(DowntimeObservation {
        height: checkpoint.height,
        block_hash: checkpoint.block_hash,
        state_root: checkpoint.state_root,
        accounts,
    })
}

fn agreed_account<'a>(
    expected: &[u16],
    observed: &'a [(u16, DowntimeAccounting)],
) -> Result<&'a DowntimeAccounting> {
    ensure!(!expected.is_empty(), "empty expected survivor set");
    ensure!(
        expected.iter().copied().collect::<BTreeSet<_>>().len() == expected.len(),
        "duplicate expected RPC"
    );
    ensure!(
        observed
            .iter()
            .map(|(port, _)| *port)
            .eq(expected.iter().copied()),
        "omitted, duplicate or unexpected survivor RPC"
    );
    let first = &observed[0].1;
    ensure!(
        observed.iter().all(|(_, account)| account == first),
        "finalized accounting disagreement"
    );
    Ok(first)
}

fn validate_top_up(before: &DowntimeAccounting, after: &DowntimeAccounting) -> Result<()> {
    let amount = checked_whole_coen_to_native(U256::from(TOP_UP_COEN))
        .ok_or_else(|| eyre!("top-up native amount overflow"))?;
    let mut expected = before.clone();
    expected.stake = before
        .stake
        .checked_add(amount)
        .ok_or_else(|| eyre!("stake overflow"))?;
    expected.mirrored_stake = before
        .mirrored_stake
        .checked_add(amount)
        .ok_or_else(|| eyre!("mirror overflow"))?;
    expected.total_staked = before
        .total_staked
        .checked_add(amount)
        .ok_or_else(|| eyre!("total overflow"))?;
    expected.staking_balance = before
        .staking_balance
        .checked_add(amount)
        .ok_or_else(|| eyre!("balance overflow"))?;
    ensure!(after == &expected, "top-up finalized accounting mismatch");
    Ok(())
}

fn validate_penalty(
    before: &DowntimeAccounting,
    after: &DowntimeAccounting,
    percent: u64,
) -> Result<()> {
    ensure!(
        before.status == 2 && before.unbonding_head.is_zero(),
        "invalid pre-fault state"
    );
    ensure!(
        before.stake == before.mirrored_stake,
        "pre-fault mirror mismatch"
    );
    ensure!((1..=100).contains(&percent), "invalid slash percent");
    let burn = before
        .stake
        .checked_mul(U256::from(percent))
        .ok_or_else(|| eyre!("burn multiplication overflow"))?
        / U256::from(100);
    ensure!(!burn.is_zero(), "slash must burn nonzero value");
    let mut expected = before.clone();
    expected.stake = before
        .stake
        .checked_sub(burn)
        .ok_or_else(|| eyre!("stake underflow"))?;
    expected.mirrored_stake = expected.stake;
    expected.total_staked = before
        .total_staked
        .checked_sub(burn)
        .ok_or_else(|| eyre!("total underflow"))?;
    expected.staking_balance = before
        .staking_balance
        .checked_sub(burn)
        .ok_or_else(|| eyre!("balance underflow"))?;
    expected.status = 6;
    expected.slash_count = before
        .slash_count
        .checked_add(1)
        .ok_or_else(|| eyre!("slash count overflow"))?;
    expected.felony_count = before
        .felony_count
        .checked_add(1)
        .ok_or_else(|| eyre!("felony count overflow"))?;
    ensure!(after == &expected, "downtime penalty accounting mismatch");
    Ok(())
}

fn validate_idempotence(
    ports: &[u16],
    penalty: &DowntimeObservation,
    later: &DowntimeObservation,
) -> Result<()> {
    let target = penalty
        .height
        .checked_add(36)
        .ok_or_else(|| eyre!("height overflow"))?;
    ensure!(
        later.height >= target,
        "insufficient finalized progress after felony"
    );
    ensure!(
        agreed_account(ports, &penalty.accounts)? == agreed_account(ports, &later.accounts)?,
        "repeated punitive accounting"
    );
    Ok(())
}

fn quantity(value: &Value) -> Result<u64> {
    let word: U256 = serde_json::from_value(value.clone())?;
    Ok(word.try_into()?)
}

fn decode_event(
    logs: &Value,
    victim: Address,
    from: u64,
    to: u64,
    threshold: u64,
) -> Result<DowntimeFelonyEvent> {
    let logs = logs
        .as_array()
        .ok_or_else(|| eyre!("event response is not an array"))?;
    ensure!(logs.len() == 1, "expected exactly one VoterFelony event");
    let log = &logs[0];
    let address: Address = serde_json::from_value(log["address"].clone())?;
    ensure!(
        address == addresses::SLASH_ADDR,
        "wrong felony event emitter"
    );
    ensure!(
        log["removed"].as_bool() == Some(false),
        "removed or unqualified felony event"
    );
    let topics: Vec<B256> = serde_json::from_value(log["topics"].clone())?;
    let data: Bytes = serde_json::from_value(log["data"].clone())?;
    ensure!(
        topics.len() == 2 && data.len() == 64,
        "malformed felony event shape"
    );
    let event = eth::ISlashIndicator::VoterFelony::decode_raw_log_validate(topics, &data)?;
    ensure!(event.validator == victim, "wrong felony event validator");
    ensure!(
        threshold > 0 && event.missCount > 0 && event.missCount.is_multiple_of(threshold),
        "felony event miss count does not reach configured threshold"
    );
    let height = quantity(&log["blockNumber"])?;
    ensure!(
        (from..=to).contains(&height),
        "felony event outside pinned range"
    );
    Ok(DowntimeFelonyEvent {
        validator: event.validator,
        miss_count: event.missCount,
        felony_count: event.felonyCount,
        height,
        block_hash: serde_json::from_value(log["blockHash"].clone())?,
        transaction_hash: serde_json::from_value(log["transactionHash"].clone())?,
        log_index: quantity(&log["logIndex"])?,
    })
}

fn observe_event(
    world: &mut World,
    state: &DowntimeState,
    ports: &[u16],
    to: u64,
) -> Result<DowntimeFelonyEvent> {
    let from = state
        .before
        .height
        .checked_add(1)
        .ok_or_else(|| eyre!("event range overflow"))?;
    let mut expected = None;
    for &port in ports {
        let logs = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getLogs",
            json!([{
                "address": addresses::SLASH_ADDR,
                "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{to:x}"),
                "topics": [eth::ISlashIndicator::VoterFelony::SIGNATURE_HASH, state.victim.into_word()],
            }]),
        )?;
        retain(
            world,
            "downtime_felony_logs",
            json!({"port": port, "from": from, "to": to, "logs": logs}),
        );
        let event = decode_event(&logs, state.victim, from, to, state.threshold)?;
        ensure!(
            world.rpc.finalized_result(port)? >= to,
            "event range is not finalized"
        );
        let canonical = world.rpc.checkpoint_at(port, event.height)?;
        validate_event_checkpoint(&event, canonical)?;
        if let Some(ref expected) = expected {
            ensure!(
                &event == expected,
                "survivors disagree on felony event identity"
            );
        } else {
            expected = Some(event);
        }
    }
    expected.ok_or_else(|| eyre!("no expected event observers"))
}

fn validate_event_checkpoint(
    event: &DowntimeFelonyEvent,
    canonical: FinalizedCheckpoint,
) -> Result<()> {
    ensure!(
        event.height == canonical.height,
        "felony event canonical height mismatch"
    );
    ensure!(
        event.block_hash == canonical.block_hash,
        "felony event has noncanonical block hash"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> DowntimeAccounting {
        let stake = checked_whole_coen_to_native(U256::from(100_100)).unwrap();
        let total = checked_whole_coen_to_native(U256::from(400_100)).unwrap();
        DowntimeAccounting {
            stake,
            mirrored_stake: stake,
            total_staked: total,
            staking_balance: total,
            status: 2,
            slash_count: 7,
            felony_count: 9,
            unbonding_head: U256::ZERO,
            slash_config: [U256::from(5), U256::from(30)],
        }
    }

    fn topped_up(before: &DowntimeAccounting, amount: U256) -> DowntimeAccounting {
        let mut after = before.clone();
        after.stake += amount;
        after.mirrored_stake += amount;
        after.total_staked += amount;
        after.staking_balance += amount;
        after
    }

    fn penalized(before: &DowntimeAccounting) -> DowntimeAccounting {
        let mut after = before.clone();
        let burn = before.stake * U256::from(5) / U256::from(100);
        after.stake -= burn;
        after.mirrored_stake -= burn;
        after.total_staked -= burn;
        after.staking_balance -= burn;
        after.status = 6;
        after.slash_count += 1;
        after.felony_count += 1;
        after
    }

    fn observation(height: u64, account: &DowntimeAccounting) -> DowntimeObservation {
        DowntimeObservation {
            height,
            block_hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
            accounts: [8100, 8200, 8300]
                .into_iter()
                .map(|port| (port, account.clone()))
                .collect(),
        }
    }

    fn felony_log() -> Value {
        let event = eth::ISlashIndicator::VoterFelony {
            validator: Address::repeat_byte(3),
            missCount: 30,
            felonyCount: 10,
        };
        let data = event.encode_log_data();
        json!({
            "address": addresses::SLASH_ADDR, "topics": data.topics(), "data": data.data,
            "removed": false, "blockNumber": "0x28", "blockHash": B256::repeat_byte(1),
            "transactionHash": B256::repeat_byte(4), "logIndex": "0x0",
        })
    }

    #[test]
    fn top_up_uses_canonical_eighteen_decimal_units() {
        let amount: U256 = "100000000000000000000".parse().unwrap();
        assert_eq!(
            checked_whole_coen_to_native(U256::from(TOP_UP_COEN)),
            Some(amount)
        );
        let before = baseline();
        assert!(validate_top_up(&before, &topped_up(&before, amount)).is_ok());
        for wrong in [U256::from(100), U256::from(100_000_000)] {
            assert!(validate_top_up(&before, &topped_up(&before, wrong)).is_err());
        }
        let mut overflowing = before.clone();
        overflowing.stake = U256::MAX;
        assert!(validate_top_up(&overflowing, &before).is_err());
    }

    #[test]
    fn top_up_checks_all_four_accounting_surfaces() {
        let before = baseline();
        let amount = checked_whole_coen_to_native(U256::from(TOP_UP_COEN)).unwrap();
        for field in 0..4 {
            let mut after = topped_up(&before, amount);
            match field {
                0 => after.stake -= U256::ONE,
                1 => after.mirrored_stake -= U256::ONE,
                2 => after.total_staked -= U256::ONE,
                _ => after.staking_balance -= U256::ONE,
            }
            assert!(validate_top_up(&before, &after).is_err(), "field {field}");
        }
    }

    #[test]
    fn penalty_requires_exact_burn_both_counters_status_and_unchanged_config() {
        let before = baseline();
        let after = penalized(&before);
        assert!(validate_penalty(&before, &after, 5).is_ok());
        for field in 0..9 {
            let mut wrong = after.clone();
            match field {
                0 => wrong.stake += U256::ONE,
                1 => wrong.mirrored_stake += U256::ONE,
                2 => wrong.total_staked += U256::ONE,
                3 => wrong.staking_balance += U256::ONE,
                4 => wrong.slash_count += 1,
                5 => wrong.felony_count += 1,
                6 => wrong.status = 2,
                7 => wrong.unbonding_head = U256::ONE,
                _ => wrong.slash_config[0] += U256::ONE,
            }
            assert!(
                validate_penalty(&before, &wrong, 5).is_err(),
                "field {field}"
            );
        }
    }

    #[test]
    fn penalty_rejects_unbonding_zero_burn_and_invalid_arithmetic() {
        let mut before = baseline();
        let after = penalized(&before);
        before.unbonding_head = U256::ONE;
        assert!(validate_penalty(&before, &after, 5).is_err());
        before = baseline();
        for percent in [0, 101] {
            assert!(validate_penalty(&before, &after, percent).is_err());
        }
        before.stake = U256::ONE;
        before.mirrored_stake = U256::ONE;
        assert!(validate_penalty(&before, &after, 5).is_err());
        before.stake = U256::MAX;
        before.mirrored_stake = U256::MAX;
        assert!(validate_penalty(&before, &after, 5).is_err());
        before = baseline();
        before.slash_count = u64::MAX;
        assert!(validate_penalty(&before, &after, 5).is_err());
    }

    #[test]
    fn survivor_agreement_rejects_omission_duplicates_extra_and_divergence() {
        let expected = [8100, 8200, 8300];
        let observed = observation(40, &baseline());
        assert!(agreed_account(&expected, &observed.accounts).is_ok());
        assert!(agreed_account(&expected, &observed.accounts[..2]).is_err());
        assert!(agreed_account(&[], &[]).is_err());
        assert!(agreed_account(&[8100, 8100], &observed.accounts[..2]).is_err());
        let mut wrong = observed.accounts.clone();
        wrong[2].0 = 8200;
        assert!(agreed_account(&expected, &wrong).is_err());
        wrong = observed.accounts.clone();
        wrong.push((8400, baseline()));
        assert!(agreed_account(&expected, &wrong).is_err());
        wrong = observed.accounts.clone();
        wrong[2].1.staking_balance += U256::ONE;
        assert!(agreed_account(&expected, &wrong).is_err());
    }

    #[test]
    fn felony_event_requires_exactly_one_decodable_victim_log() {
        let victim = Address::repeat_byte(3);
        let log = felony_log();
        assert!(decode_event(&json!([log]), victim, 4, 40, 30).is_ok());
        assert!(decode_event(&json!([]), victim, 4, 40, 30).is_err());
        assert!(decode_event(&json!([log, log]), victim, 4, 40, 30).is_err());
        assert!(decode_event(&json!([log]), Address::ZERO, 4, 40, 30).is_err());
        for (field, value) in [
            ("address", json!(Address::ZERO)),
            ("removed", json!(true)),
            ("data", json!("0x01")),
            ("topics", json!([])),
            ("blockNumber", json!("0x29")),
            ("blockHash", Value::Null),
            ("transactionHash", Value::Null),
            ("logIndex", Value::Null),
        ] {
            let mut wrong = log.clone();
            wrong[field] = value;
            assert!(
                decode_event(&json!([wrong]), victim, 4, 40, 30).is_err(),
                "field {field}"
            );
        }
        assert!(decode_event(&json!([log]), victim, 41, 50, 30).is_err());
        assert!(decode_event(&json!([log]), victim, 4, 40, 0).is_err());
        assert!(decode_event(&json!([log]), victim, 4, 40, 31).is_err());
    }

    #[test]
    fn felony_event_binds_canonical_height_and_hash() {
        let event =
            decode_event(&json!([felony_log()]), Address::repeat_byte(3), 4, 40, 30).unwrap();
        let checkpoint = FinalizedCheckpoint {
            height: 40,
            block_hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
        };
        assert!(validate_event_checkpoint(&event, checkpoint).is_ok());
        assert!(validate_event_checkpoint(
            &event,
            FinalizedCheckpoint {
                height: 41,
                ..checkpoint
            }
        )
        .is_err());
        assert!(validate_event_checkpoint(
            &event,
            FinalizedCheckpoint {
                block_hash: B256::ZERO,
                ..checkpoint
            }
        )
        .is_err());
    }

    #[test]
    fn idempotence_requires_thirty_six_finalized_blocks_and_all_accounting() {
        let ports = [8100, 8200, 8300];
        let after = penalized(&baseline());
        let penalty = observation(40, &after);
        assert!(validate_idempotence(&ports, &penalty, &observation(76, &after)).is_ok());
        assert!(validate_idempotence(&ports, &penalty, &observation(75, &after)).is_err());
        for field in 0..6 {
            let mut changed = after.clone();
            match field {
                0 => changed.stake -= U256::ONE,
                1 => changed.mirrored_stake -= U256::ONE,
                2 => changed.total_staked -= U256::ONE,
                3 => changed.staking_balance -= U256::ONE,
                4 => changed.slash_count += 1,
                _ => changed.felony_count += 1,
            }
            assert!(validate_idempotence(&ports, &penalty, &observation(76, &changed)).is_err());
        }
        let mut missing = observation(76, &after);
        missing.accounts.pop();
        assert!(validate_idempotence(&ports, &penalty, &missing).is_err());
    }
}
