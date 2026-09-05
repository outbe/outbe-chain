//! Evidence-driven validator lifecycle consistency scenarios.
//!
//! These steps deliberately submit public SlashIndicator/Staking transactions.
//! They do not patch storage or call runtime internals. All target invariants in
//! this module are enforced by the main suite.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{keccak256, Address, U256};
use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::validator_evidence::{conflicting_notarize_for_validator, ConflictingNotarizeEvidence};
use crate::world::rpc::TxOutcome;
use crate::world::World;

const STATUS_PENDING: u8 = 1;
const STATUS_ACTIVE: u8 = 2;
const STATUS_EXITING: u8 = 3;
const STATUS_JAILED: u8 = 6;
const SNAPSHOT_RETAIN_EPOCHS: u64 = 8;
const EVIDENCE_VIEW: u64 = 7;

#[derive(Clone, Copy, Debug)]
struct EvidenceEvent {
    validator: Address,
    submitter: Address,
    slashed_amount: U256,
    submitter_reward: U256,
}

fn primary(world: &World) -> u16 {
    world.validators.primary_port()
}

fn validator_key_and_address(world: &World, index: usize) -> (String, String) {
    let key = world
        .validators
        .get(index)
        .evm_key()
        .unwrap_or_else(|error| panic!("read validator-{index} EOA key: {error}"));
    let address = world
        .rpc
        .address_of(&key)
        .unwrap_or_else(|| panic!("derive validator-{index} EOA address"));
    (key, address)
}

fn evidence_for(world: &World, victim_index: usize, epoch: u64) -> ConflictingNotarizeEvidence {
    conflicting_notarize_for_validator(
        &world.rpc,
        primary(world),
        &world.validators.get(victim_index),
        epoch,
        EVIDENCE_VIEW,
    )
    .unwrap_or_else(|error| panic!("construct canonical conflicting-notarize evidence: {error}"))
}

fn wait_finalized_outcome(world: &World, outcome: &TxOutcome) {
    world
        .rpc
        .finalize_outcome(outcome, &[primary(world)], 30)
        .expect("evidence transaction must have a canonical finalized outcome");
}

fn parse_evidence_event(outcome: &TxOutcome) -> Option<EvidenceEvent> {
    let signature = format!(
        "{:#x}",
        keccak256("EvidenceFelonyApplied(address,address,uint256,uint256)")
    );
    let logs = outcome.receipt.get("logs")?.as_array()?;
    let log = logs.iter().find(|log| {
        log.get("topics")
            .and_then(serde_json::Value::as_array)
            .and_then(|topics| topics.first())
            .and_then(serde_json::Value::as_str)
            .is_some_and(|topic| topic.eq_ignore_ascii_case(&signature))
    })?;
    let topics = log.get("topics")?.as_array()?;
    let validator = indexed_address(topics.get(1)?.as_str()?)?;
    let submitter = indexed_address(topics.get(2)?.as_str()?)?;
    let data = log.get("data")?.as_str()?.trim_start_matches("0x");
    let bytes = hex::decode(data).ok()?;
    if bytes.len() != 64 {
        return None;
    }
    Some(EvidenceEvent {
        validator,
        submitter,
        slashed_amount: U256::from_be_slice(&bytes[..32]),
        submitter_reward: U256::from_be_slice(&bytes[32..]),
    })
}

fn indexed_address(topic: &str) -> Option<Address> {
    let bytes = hex::decode(topic.trim_start_matches("0x")).ok()?;
    (bytes.len() == 32).then(|| Address::from_slice(&bytes[12..]))
}

fn submit_initial_felony(world: &mut World) {
    // A compact but valid rotation schedule lets the continuation of D-10
    // observe exclusion/recovery without changing any protocol invariant.
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "30".to_owned()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "10".to_owned()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "10".to_owned()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "29".to_owned()),
        ],
    );

    let port = primary(world);
    let (_, victim) = validator_key_and_address(world, 3);
    let (reporter_key, reporter) = validator_key_and_address(world, 0);
    let before = world
        .rpc
        .validator_record(port, &victim)
        .expect("read active victim before evidence");
    assert_eq!(before.status, STATUS_ACTIVE, "victim must start ACTIVE");
    assert!(before.has_bls_share, "victim must start with a BLS share");
    assert!(
        world
            .rpc
            .is_participant(port, &victim)
            .expect("observe consensus participation"),
        "victim must start as a consensus participant"
    );
    assert_eq!(
        world.rpc.validator_status(port, &reporter),
        Some(u64::from(STATUS_ACTIVE)),
        "evidence reporter must be ACTIVE"
    );

    let epoch = world.rpc.epoch_on(port).expect("read evidence epoch");
    let evidence = evidence_for(world, 3, epoch);
    let outcome = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block1, &evidence.block2)
        .expect("submit valid felony evidence");
    assert!(
        outcome.success,
        "valid conflicting-notarize evidence must be accepted: {}",
        outcome.transaction_hash
    );
    wait_finalized_outcome(world, &outcome);

    let after = world
        .rpc
        .validator_record(port, &victim)
        .expect("read jailed victim after evidence");
    assert_eq!(
        after.status, STATUS_JAILED,
        "valid felony evidence must jail the active validator"
    );
    assert!(
        after.has_bls_share,
        "pre-boundary jail setup must retain the current live share"
    );
    assert_eq!(
        after.slash_count,
        before.slash_count + 1,
        "the setup felony must increment slash_count exactly once"
    );
    assert!(
        after.stake < before.stake,
        "the setup felony must slash bonded stake"
    );

    world.state.joiner_addr = Some(victim);
    world.state.wwd = Some(reporter);
    world.state.proposed_version = Some(epoch);
    world.state.activation_height = Some(EVIDENCE_VIEW);
    world.state.slash_stake_after = Some(after.stake);
    // A jail already records deactivation height. D-07 verifies that its
    // partial unstake does not create a second exit transition.
    world.state.marker_height = Some(after.deactivated_at_height);
}

#[given("valid conflicting-notarize evidence for an active validator and its canonical reverse")]
fn valid_evidence_and_reverse(world: &mut World) {
    boot_localnet(world, 6, &[]);

    let port = primary(world);
    let (_, victim) = validator_key_and_address(world, 3);
    let (_, reporter) = validator_key_and_address(world, 0);
    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("read evidence victim");
    assert_eq!(record.status, STATUS_ACTIVE, "victim must start ACTIVE");
    assert!(record.has_bls_share, "victim must have a current share");
    assert_eq!(
        world.rpc.validator_status(port, &reporter),
        Some(u64::from(STATUS_ACTIVE)),
        "reporter must be ACTIVE"
    );

    let epoch = world.rpc.epoch_on(port).expect("read evidence epoch");
    let evidence = evidence_for(world, 3, epoch);
    assert_ne!(
        evidence.block1, evidence.block2,
        "the two notarize proposals must conflict"
    );

    let from_block = world
        .rpc
        .finalized(port)
        .or_else(|| world.rpc.head(port))
        .expect("read pre-evidence finalized height");
    assert_eq!(
        world
            .rpc
            .evidence_felony_event_count(port, &victim, from_block)
            .expect("observe fresh finalized evidence range"),
        0,
        "fresh evidence range must contain no prior felony event"
    );

    world.state.joiner_addr = Some(victim.clone());
    world.state.wwd = Some(reporter.clone());
    world.state.marker_height = Some(from_block);
    world.state.proposed_version = Some(epoch);
    world.state.activation_height = Some(EVIDENCE_VIEW);
    world.state.slash_stake_before = Some(record.stake);
    world.state.slash_count_before = Some(record.slash_count);
    world.state.slash_total_before = world.rpc.total_staked_on(port);
    world.state.slash_staking_balance_before = world.rpc.staking_balance_on(port);
    world.state.marker_count = world
        .rpc
        .felony_count(port, &victim)
        .and_then(|count| usize::try_from(count).ok());
    world.state.lifecycle_stake_before_exit = world.rpc.balance_on(port, &reporter);
}

#[when("a reporter submits the evidence and then submits every replay")]
fn reporter_submits_evidence_and_replays(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("victim address");
    let reporter = world.state.wwd.clone().expect("reporter address");
    let epoch = world.state.proposed_version.expect("evidence epoch");
    let evidence = evidence_for(world, 3, epoch);
    let reporter_key = world.validators.get(0).evm_key().expect("reporter key");
    let reporter_address: Address = reporter.parse().expect("parse reporter address");
    let victim_address: Address = victim.parse().expect("parse victim address");

    let first = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block1, &evidence.block2)
        .expect("submit first canonical evidence");
    let first_revert = (!first.success).then(|| {
        world
            .rpc
            .simulate_conflicting_notarize_evidence(
                reporter_address,
                &evidence.block1,
                &evidence.block2,
            )
            .expect_err("failed evidence receipt must also revert under eth_call")
            .to_string()
    });
    assert!(
        first.success,
        "the first valid evidence transaction must succeed: tx={} receipt={} simulation={:?}",
        first.transaction_hash, first.receipt, first_revert
    );
    let event = parse_evidence_event(&first).expect("decode EvidenceFelonyApplied event");
    assert_eq!(event.validator, victim_address, "event victim");
    assert_eq!(event.submitter, reporter_address, "event reporter");
    assert!(
        !event.slashed_amount.is_zero(),
        "the first evidence must slash a positive amount"
    );
    assert!(
        !event.submitter_reward.is_zero(),
        "the first evidence must reward its reporter"
    );
    wait_finalized_outcome(world, &first);

    let reporter_before = world
        .state
        .lifecycle_stake_before_exit
        .expect("reporter balance before evidence");
    let first_fee = first.gas_cost().expect("first evidence gas cost");
    let reporter_after_first = world
        .rpc
        .balance_on(port, &reporter)
        .expect("reporter balance after first evidence");
    assert_eq!(
        reporter_after_first + first_fee,
        reporter_before + event.submitter_reward,
        "reporter balance must change only by its emitted reward minus exact gas"
    );

    let exact_replay = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block1, &evidence.block2)
        .expect("submit exact evidence replay");
    assert!(
        !exact_replay.success,
        "the exact evidence replay must revert"
    );
    let exact_fee = exact_replay.gas_cost().expect("exact replay gas cost");
    let reporter_after_exact = world
        .rpc
        .balance_on(port, &reporter)
        .expect("reporter balance after exact replay");
    assert_eq!(
        reporter_after_exact + exact_fee,
        reporter_after_first,
        "an exact replay may charge gas but must not mint another reward"
    );

    let reverse_replay = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block2, &evidence.block1)
        .expect("submit canonical reverse replay");
    assert!(
        !reverse_replay.success,
        "the canonical reverse evidence replay must revert"
    );
    let reverse_fee = reverse_replay.gas_cost().expect("reverse replay gas cost");
    let reporter_after_reverse = world
        .rpc
        .balance_on(port, &reporter)
        .expect("reporter balance after reverse replay");
    assert_eq!(
        reporter_after_reverse + reverse_fee,
        reporter_after_exact,
        "a canonical reverse replay may charge gas but must not mint another reward"
    );

    world.state.slash_stake_after = world.rpc.stake_on(port, &victim);
    // Reuse the lifecycle accounting slots for the two decoded event values.
    // They are per-scenario scratch fields and D-05 does not run lifecycle exit.
    world.state.lifecycle_total_before_exit = Some(event.slashed_amount);
    world.state.lifecycle_staking_balance_before_exit = Some(event.submitter_reward);
}

#[then(
    "exactly one punishment changes status, stake, slash count, burn, reporter reward, and events"
)]
fn exactly_one_punishment_changes_all_surfaces(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("victim address");
    let from_block = world.state.marker_height.expect("event range start");
    let before_stake = world.state.slash_stake_before.expect("stake before");
    let after_stake = world.state.slash_stake_after.expect("stake after");
    let slashed_amount = world
        .state
        .lifecycle_total_before_exit
        .expect("event slashed amount");
    let reward = world
        .state
        .lifecycle_staking_balance_before_exit
        .expect("event reporter reward");
    let before_total = world.state.slash_total_before.expect("total before");
    let before_staking_balance = world
        .state
        .slash_staking_balance_before
        .expect("Staking balance before");
    let before_slash_count = world.state.slash_count_before.expect("slash count before");
    let before_felony_count = u64::try_from(world.state.marker_count.expect("felony count before"))
        .expect("felony count fits u64");

    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("victim after all evidence submissions");
    assert_eq!(record.status, STATUS_JAILED, "one felony must jail");
    assert_eq!(
        record.slash_count,
        before_slash_count + 1,
        "slash_count must change exactly once"
    );
    assert_eq!(
        after_stake,
        before_stake - slashed_amount,
        "stake must be reduced by the event's exact slash once"
    );
    assert_eq!(
        record.stake, after_stake,
        "ValidatorSet stake mirror after evidence replays"
    );
    assert_eq!(
        world.rpc.total_staked_on(port),
        Some(before_total - slashed_amount),
        "total_staked must burn the slash exactly once"
    );
    assert_eq!(
        world.rpc.staking_balance_on(port),
        Some(before_staking_balance - slashed_amount),
        "Staking native balance must burn the slash exactly once"
    );
    assert_eq!(
        world.rpc.felony_count(port, &victim),
        Some(before_felony_count + 1),
        "SlashIndicator felony count must change exactly once"
    );
    assert_eq!(
        world
            .rpc
            .evidence_felony_event_count(port, &victim, from_block)
            .expect("observe finalized evidence application range"),
        1,
        "exactly one EvidenceFelonyApplied event must be finalized"
    );
    assert!(
        !reward.is_zero(),
        "the one accepted punishment must carry one positive reporter reward"
    );
}

#[then("every replay is rejected without another punishment state change")]
fn every_replay_is_rejected_without_state_change(world: &mut World) {
    // Rejection and reporter-balance checks happen immediately after each
    // receipt in the When step.  Re-read the coupled punitive fields here so a
    // delayed/finalized replay effect cannot hide behind receipt ordering.
    exactly_one_punishment_changes_all_surfaces(world);
}

#[given("an active validator is jailed by valid felony evidence")]
fn active_validator_is_jailed_by_evidence(world: &mut World) {
    submit_initial_felony(world);
}

fn jailed_partial_unstake_remainder(stake: U256, minimum_stake: U256) -> Option<U256> {
    let bounded = stake.min(minimum_stake);
    let remainder = bounded / U256::from(2_u8);
    (!remainder.is_zero()).then_some(remainder)
}

#[when("it unstakes below minimum while leaving a nonzero bonded amount")]
fn jailed_validator_partially_unstakes(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("jailed victim");
    let victim_key = world.validators.get(3).evm_key().expect("victim key");
    let stake = world
        .rpc
        .stake_on(port, &victim)
        .expect("jailed stake before partial unstake");
    let remaining = jailed_partial_unstake_remainder(stake, crate::internal::eth::coen(1_000))
        .expect("jailed validator must retain a positive partial stake");
    assert!(
        stake > remaining,
        "jailed validator must have enough stake to leave a nonzero below-minimum remainder"
    );
    let outcome = world
        .rpc
        .unstake_base_units(&victim_key, stake - remaining)
        .expect("submit jailed partial unstake");
    assert!(
        outcome.success,
        "the public partial unstake must reach its lifecycle transition"
    );
    world.state.slash_stake_after = Some(remaining);
}

#[then("it remains JAILED with the remaining stake and no exit transition")]
fn partial_jailed_unstake_does_not_exit(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("jailed victim");
    let remaining = world
        .state
        .slash_stake_after
        .expect("remaining jailed stake");
    let jailed_deactivation = world.state.marker_height.expect("jail transition height");
    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("victim after partial jailed unstake");

    // D-07 target invariant: a nonzero bonded amount retains the jail state.
    assert_eq!(
        record.status, STATUS_JAILED,
        "D-07 target invariant: partial jailed unstake must remain JAILED, got status {} (EXITING={STATUS_EXITING})",
        record.status
    );
    assert_eq!(record.stake, remaining, "remaining bonded stake");
    assert_eq!(
        world.rpc.stake_on(port, &victim),
        Some(remaining),
        "Staking stake must match ValidatorSet"
    );
    assert_eq!(
        record.deactivated_at_height, jailed_deactivation,
        "partial unstake must not create another exit transition"
    );
}

#[when("it requests unjail before an exclusion boundary")]
fn requests_early_unjail(world: &mut World) {
    let victim_key = world.validators.get(3).evm_key().expect("victim key");
    let outcome = world
        .rpc
        .unjail_validator(&victim_key)
        .expect("submit early unjail");
    world.state.marker_count = Some(outcome.success as usize);
}

#[then("early unjail is rejected and preserves the retained jailed committee member")]
fn early_unjail_is_rejected_while_retained(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("jailed victim");
    let accepted = world.state.marker_count == Some(1);
    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("victim after early unjail");

    assert!(!accepted, "retained committee member accepted early unjail");
    assert_eq!(
        record.status, STATUS_JAILED,
        "reverted early unjail must preserve JAILED"
    );
    assert!(
        record.has_bls_share,
        "retained jailed committee member lost its live BLS share before exclusion"
    );
    assert!(
        world
            .rpc
            .is_participant(port, &victim)
            .expect("observe consensus participation"),
        "retained jailed committee member disappeared before exclusion boundary"
    );
}

#[then(
    "the validator cannot participate until readiness is reconfirmed and a fresh reshare commits"
)]
fn early_unjail_requires_reconfirmation_and_reshare(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("jailed victim");
    let victim_key = world.validators.get(3).evm_key().expect("victim key");

    assert_eq!(
        world.state.marker_count,
        Some(0),
        "early unjail must revert"
    );
    wait_until(
        || {
            !world
                .rpc
                .is_participant(port, &victim)
                .expect("observe consensus participation")
        },
        120,
        "jailed validator exclusion boundary",
    );
    let outcome = world
        .rpc
        .unjail_validator(&victim_key)
        .expect("unjail after exclusion boundary");
    assert!(outcome.success, "post-exclusion unjail must succeed");

    let unconfirmed_epoch = world.rpc.epoch_on(port).expect("epoch after unjail");
    wait_until(
        || {
            world
                .rpc
                .epoch_on(port)
                .is_some_and(|epoch| epoch > unconfirmed_epoch)
        },
        120,
        "an unconfirmed post-unjail epoch",
    );
    assert_eq!(
        world.rpc.validator_status(port, &victim),
        Some(u64::from(STATUS_PENDING)),
        "unjail must remain PENDING without readiness confirmation"
    );
    assert!(
        !world
            .rpc
            .is_participant(port, &victim)
            .expect("observe consensus participation"),
        "unconfirmed PENDING validator must stay excluded"
    );

    let confirmation = world
        .rpc
        .confirm_ready_outcome(&victim_key, 3)
        .expect("reconfirm validator readiness");
    assert!(
        confirmation.success,
        "readiness reconfirmation must succeed"
    );
    wait_until(
        || {
            world
                .rpc
                .is_participant(port, &victim)
                .expect("observe consensus participation")
        },
        180,
        "fresh reshare after readiness reconfirmation",
    );
    let recovered = world
        .rpc
        .validator_record(port, &victim)
        .expect("validator after recovery reshare");
    assert_eq!(recovered.status, STATUS_ACTIVE, "recovered status");
    assert!(recovered.has_bls_share, "recovered validator share");
}

#[given("valid conflicting-notarize evidence is retained for the current epoch")]
fn retain_current_epoch_evidence(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            // Eight retained snapshots cover seven epoch intervals. Keep that
            // span above the mandatory OCOMP job lifetime: 7 * 30 > 188.
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "30".to_owned()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "16".to_owned()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "12".to_owned()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "23".to_owned()),
        ],
    );

    let port = primary(world);
    let (_, victim) = validator_key_and_address(world, 3);
    let (_, reporter) = validator_key_and_address(world, 0);
    let epoch = world.rpc.epoch_on(port).expect("retained evidence epoch");
    let _ = evidence_for(world, 3, epoch);
    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("victim before snapshot retention wrap");
    assert_eq!(record.status, STATUS_ACTIVE, "victim must start ACTIVE");
    assert!(record.has_bls_share, "victim must start with a share");

    world.state.joiner_addr = Some(victim);
    world.state.wwd = Some(reporter);
    world.state.proposed_version = Some(epoch);
    world.state.marker_height = world.rpc.finalized(port).or_else(|| world.rpc.head(port));
    world.state.marker_count = Some(
        world
            .localnet
            .log_count(0, "DKG reshare activated; new active set committed")
            .expect("read required owned process log"),
    );
    world.state.slash_stake_before = Some(record.stake);
    world.state.slash_count_before = Some(record.slash_count);
    world.state.slash_total_before = world.rpc.total_staked_on(port);
    world.state.slash_staking_balance_before = world.rpc.staking_balance_on(port);
    world.state.lifecycle_stake_before_exit = world
        .rpc
        .felony_count(port, world.state.joiner_addr.as_deref().unwrap())
        .map(U256::from);
}

#[when("the unchanged committee advances beyond the snapshot-ring retention window")]
fn advance_beyond_snapshot_ring(world: &mut World) {
    let port = primary(world);
    let original_epoch = world.state.proposed_version.expect("retained epoch");
    let target_epoch = original_epoch + SNAPSHOT_RETAIN_EPOCHS;
    let initial_boundaries = world.state.marker_count.expect("initial boundary count");
    let expected_committee = expected_genesis_committee(world);

    wait_until(
        || {
            let epoch_advanced = world
                .rpc
                .epoch_on(port)
                .is_some_and(|epoch| epoch >= target_epoch);
            let rotations = world
                .localnet
                .log_count(0, "DKG reshare activated; new active set committed")
                .expect("read required owned process log");
            epoch_advanced && rotations >= initial_boundaries + SNAPSHOT_RETAIN_EPOCHS as usize
        },
        360,
        "eight successful same-membership boundaries and snapshot-ring wrap",
    );

    let mut actual = world
        .rpc
        .active_consensus_set(port)
        .expect("active consensus set after retention wrap");
    actual.sort_unstable();
    assert_eq!(
        actual, expected_committee,
        "S-12 setup requires the committee to remain unchanged across the ring wrap"
    );
}

#[when("the old evidence is submitted after its epoch slot has been reused")]
fn submit_evicted_epoch_evidence(world: &mut World) {
    let epoch = world.state.proposed_version.expect("retained epoch");
    let evidence = evidence_for(world, 3, epoch);
    let reporter_key = world.validators.get(0).evm_key().expect("reporter key");
    let outcome = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block1, &evidence.block2)
        .expect("submit evicted-epoch evidence");
    world.state.marker_count = Some(outcome.success as usize);
}

#[then("the evicted evidence is rejected without punishment or accounting changes")]
fn evicted_evidence_is_rejected_atomically(world: &mut World) {
    let port = primary(world);
    let victim = world.state.joiner_addr.clone().expect("victim address");
    let accepted = world.state.marker_count == Some(1);

    // S-12 target invariant: even with an unchanged committee, the epoch-bound
    // snapshot key must reject evidence after the old ring entry is evicted.
    assert!(
        !accepted,
        "S-12 target invariant: evidence for an evicted epoch must be rejected even when the colliding snapshot has the same committee"
    );

    let record = world
        .rpc
        .validator_record(port, &victim)
        .expect("victim after rejected evicted evidence");
    assert_eq!(record.status, STATUS_ACTIVE, "status must not change");
    assert_eq!(
        record.stake,
        world.state.slash_stake_before.expect("stake before"),
        "stake must not change"
    );
    assert_eq!(
        record.slash_count,
        world.state.slash_count_before.expect("slash count before"),
        "slash_count must not change"
    );
    assert_eq!(
        world.rpc.total_staked_on(port),
        world.state.slash_total_before,
        "total_staked must not change"
    );
    assert_eq!(
        world.rpc.staking_balance_on(port),
        world.state.slash_staking_balance_before,
        "Staking native balance must not change"
    );
    assert_eq!(
        world.rpc.felony_count(port, &victim).map(U256::from),
        world.state.lifecycle_stake_before_exit,
        "felony accounting must not change"
    );
    assert_eq!(
        world
            .rpc
            .evidence_felony_event_count(
                port,
                &victim,
                world.state.marker_height.expect("event range start")
            )
            .expect("observe finalized evidence range after rejected replay"),
        0,
        "rejected evicted evidence must emit no punishment event"
    );
}

fn expected_genesis_committee(world: &World) -> Vec<Address> {
    let mut committee = (0..world.validators.size())
        .map(|index| {
            let (_, address) = validator_key_and_address(world, index);
            address
                .parse::<Address>()
                .unwrap_or_else(|error| panic!("parse validator-{index} address: {error}"))
        })
        .collect::<Vec<_>>();
    committee.sort_unstable();
    committee
}

fn wait_until(mut condition: impl FnMut() -> bool, attempts: usize, label: &str) {
    for _ in 0..attempts {
        if condition() {
            return;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(condition(), "timed out waiting for {label}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jailed_partial_unstake_remainder_is_positive_and_below_the_minimum() {
        let minimum = U256::from(1_000_000_000_000_000_000_000_u128);
        assert_eq!(
            jailed_partial_unstake_remainder(U256::from(900_000_000_000_000_000_000_u128), minimum,),
            Some(U256::from(450_000_000_000_000_000_000_u128))
        );
        assert_eq!(
            jailed_partial_unstake_remainder(
                U256::from(2_000_000_000_000_000_000_000_u128),
                minimum,
            ),
            Some(U256::from(500_000_000_000_000_000_000_u128))
        );
        assert_eq!(jailed_partial_unstake_remainder(U256::ZERO, minimum), None);
    }
}
