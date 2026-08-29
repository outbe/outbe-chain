//! Release real-SGX/no-DCAP evidence for the recurring manual TEE lease.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::internal::eth;
use crate::world::World;

const MISSED_VALIDATOR: usize = 3;
const STATUS_PENDING: u8 = 1;
const STATUS_ACTIVE: u8 = 2;
const STATUS_JAILED: u8 = 6;
const WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;
const LEASE_SECONDS: u64 = 14 * 24 * 60 * 60;

#[derive(Default)]
struct LeaseScenarioState {
    full_node_index: Option<usize>,
    original_deadline: Option<u64>,
    renewed_deadline: Option<u64>,
    missed_validator_address: Option<String>,
    missed_validator_stake: Option<U256>,
    missed_validator_slash_count: Option<u64>,
    permanent_offer_keys: Vec<[u8; 32]>,
}

fn state() -> MutexGuard<'static, LeaseScenarioState> {
    static STATE: OnceLock<Mutex<LeaseScenarioState>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(LeaseScenarioState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_until(mut predicate: impl FnMut() -> bool, attempts: u32, label: &str) {
    for _ in 0..attempts {
        if predicate() {
            return;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(predicate(), "timed out waiting for {label}");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FullNodeSyncProbeV1 {
    Pending,
    Reached(u64),
    Exited,
}

const fn classify_full_node_sync_probe(
    head: Option<u64>,
    checkpoint: u64,
    exited: bool,
) -> FullNodeSyncProbeV1 {
    if exited {
        FullNodeSyncProbeV1::Exited
    } else if let Some(height) = head {
        if height >= checkpoint {
            return FullNodeSyncProbeV1::Reached(height);
        }
        FullNodeSyncProbeV1::Pending
    } else {
        FullNodeSyncProbeV1::Pending
    }
}

fn wait_for_live_full_node_checkpoint(
    world: &mut World,
    full_node: usize,
    checkpoint: u64,
    timeout: Duration,
) -> Result<u64, String> {
    let port = world.validators.http_port(full_node);
    let node_log = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{full_node}/node.log"));
    let started = Instant::now();
    loop {
        let head = world.rpc.head(port);
        let exited = world.localnet.joiner_full_node_exited(full_node);
        match classify_full_node_sync_probe(head, checkpoint, exited) {
            FullNodeSyncProbeV1::Reached(height) => return Ok(height),
            FullNodeSyncProbeV1::Exited => {
                return Err(format!(
                    "FullNode {full_node} exited at head {head:?} before checkpoint {checkpoint}; inspect {}",
                    node_log.display()
                ));
            }
            FullNodeSyncProbeV1::Pending => {}
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "FullNode {full_node} remained alive but did not reach checkpoint {checkpoint} within {}s (last head {head:?}); inspect {}",
                timeout.as_secs(),
                node_log.display()
            ));
        }
        sleep(Duration::from_secs(2));
    }
}

fn validator_address(world: &World, index: usize) -> String {
    let key = world
        .validators
        .get(index)
        .evm_key()
        .expect("validator EVM key");
    format!(
        "{:#x}",
        eth::address_of(&key).expect("validator EVM address")
    )
}

#[given("a fresh four-validator manual TEE lease localnet")]
fn fresh_manual_lease_localnet(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "lease evidence requires 4 validators"
    );
    *state() = LeaseScenarioState::default();
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "60".to_owned()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "24".to_owned()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "24".to_owned()),
        ],
    );

    let status = world
        .localnet
        .node_renewal_status(0)
        .expect("founding validator finalized lease status");
    assert!(
        status.finalized_height > 0,
        "lease status must be finalized"
    );
    assert!(
        status.valid_until > status.finalized_timestamp,
        "founding lease must be live"
    );
    assert!(
        status.valid_until - status.finalized_timestamp <= LEASE_SECONDS,
        "founding lease cannot exceed the fourteen-day policy"
    );

    let missed_address = validator_address(world, MISSED_VALIDATOR);
    let record = world
        .rpc
        .validator_record(world.validators.primary_port(), &missed_address)
        .expect("missed validator baseline record");
    assert_eq!(
        record.status, STATUS_ACTIVE,
        "missed validator baseline status"
    );
    assert!(record.has_bls_share, "missed validator baseline share");

    let mut scenario = state();
    scenario.original_deadline = Some(status.valid_until);
    scenario.missed_validator_address = Some(missed_address);
    scenario.missed_validator_stake = Some(record.stake);
    scenario.missed_validator_slash_count = Some(record.slash_count);
}

#[given("one role-neutral FullNode joins with the committee lease deadline")]
fn full_node_joins_with_committee_deadline(world: &mut World) {
    let full_node = world.validators.joiner_index();
    let deadline = state()
        .original_deadline
        .expect("founding committee lease deadline");
    world
        .localnet
        .provision_full_node_node_host_until(full_node, deadline)
        .expect("provision role-neutral FullNode TEE identity");
    world
        .localnet
        .launch_dcap_full_node("tee-lease-full-node", full_node, 0)
        .expect("launch role-neutral FullNode");

    let checkpoint = world
        .rpc
        .finalized(world.validators.primary_port())
        .unwrap_or(1);
    wait_for_live_full_node_checkpoint(world, full_node, checkpoint, Duration::from_secs(120))
        .unwrap_or_else(|error| panic!("FullNode did not sync before lease testing: {error}"));
    let status = world
        .localnet
        .node_renewal_status(full_node)
        .expect("FullNode finalized lease status");
    assert_eq!(
        status.valid_until, deadline,
        "FullNode aligned lease deadline"
    );

    let mut offer_keys = Vec::with_capacity(5);
    for index in 0..=full_node {
        offer_keys.push(
            world
                .localnet
                .node_offer_public(index)
                .unwrap_or_else(|error| panic!("read node {index} offer key: {error:#}")),
        );
    }
    let mut scenario = state();
    scenario.full_node_index = Some(full_node);
    scenario.permanent_offer_keys = offer_keys;
}

#[when("finalized consensus time enters the final seven-day renewal window")]
fn enter_manual_renewal_window(world: &mut World) {
    let deadline = state().original_deadline.expect("original deadline");
    let opens_at = deadline
        .checked_sub(WINDOW_SECONDS)
        .expect("renewal window");
    world
        .localnet
        .restart_committee_at_consensus_timestamp(opens_at + 2)
        .expect("move committee into renewal window");
    let before = world
        .rpc
        .finalized(world.validators.primary_port())
        .unwrap_or(1);
    assert!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.primary_port(), before + 2, 180),
        "committee did not finalize inside the renewal window"
    );
    let status = world
        .localnet
        .node_renewal_status(0)
        .expect("renewal-window finalized status");
    assert!(
        status.finalized_timestamp >= opens_at,
        "window did not open"
    );
    assert!(
        status.finalized_timestamp < deadline,
        "lease already expired"
    );
}

#[when("validators 0, 1, and 2 manually renew their enclave leases")]
fn three_validators_renew(world: &mut World) {
    let original = state().original_deadline.expect("original deadline");
    let expected = original.checked_add(LEASE_SECONDS).expect("next deadline");
    for index in 0..3 {
        let outcome = world
            .localnet
            .renew_node_enclave_until_finalized(index)
            .unwrap_or_else(|error| panic!("renew validator {index}: {error:#}"));
        assert_eq!(outcome.renewal_nonce, 1, "first exact renewal nonce");
        assert_eq!(outcome.valid_until, expected, "exact next lease deadline");
    }
    state().renewed_deadline = Some(expected);
}

#[then("their exact next deadlines finalize without changing any permanent offer key")]
fn renewed_deadlines_and_keys_are_exact(world: &mut World) {
    let scenario = state();
    let original = scenario.original_deadline.expect("original deadline");
    let renewed = scenario.renewed_deadline.expect("renewed deadline");
    let full_node = scenario.full_node_index.expect("FullNode index");
    for index in 0..3 {
        let status = world
            .localnet
            .node_renewal_status(index)
            .unwrap_or_else(|error| panic!("status validator {index}: {error:#}"));
        assert_eq!(status.valid_until, renewed, "renewed finalized deadline");
        assert_eq!(status.journal_state.as_deref(), Some("finalized"));
    }
    for index in [MISSED_VALIDATOR, full_node] {
        let status = world
            .localnet
            .node_renewal_status(index)
            .unwrap_or_else(|error| panic!("status missed node {index}: {error:#}"));
        assert_eq!(status.valid_until, original, "missed node deadline changed");
        assert_eq!(status.journal_state, None, "missed node gained a journal");
    }
    for index in 0..=full_node {
        assert_eq!(
            world
                .localnet
                .node_offer_public(index)
                .unwrap_or_else(|error| panic!("read node {index} offer key: {error:#}")),
            scenario.permanent_offer_keys[index],
            "manual lease transition changed node {index} permanent offer key"
        );
    }
}

#[when("finalized consensus time reaches the original lease deadline")]
fn reach_original_deadline(world: &mut World) {
    let deadline = state().original_deadline.expect("original deadline");
    world
        .localnet
        .restart_committee_at_consensus_timestamp(deadline.saturating_sub(120))
        .expect("restart committee immediately before lease deadline");
    wait_until(
        || {
            world
                .rpc
                .validator_record(
                    world.validators.primary_port(),
                    state()
                        .missed_validator_address
                        .as_deref()
                        .expect("missed validator address"),
                )
                .is_some_and(|record| record.status == STATUS_JAILED)
        },
        240,
        "TEE-expiry jail",
    );
}

#[then("validator 3 is jailed without slash while three validators keep finalizing")]
fn missed_validator_is_jailed_without_slash(world: &mut World) {
    let scenario = state();
    let address = scenario
        .missed_validator_address
        .as_deref()
        .expect("missed validator address");
    let record = world
        .rpc
        .validator_record(world.validators.primary_port(), address)
        .expect("TEE-expired validator record");
    assert_eq!(record.status, STATUS_JAILED, "TEE-expiry jail status");
    assert_eq!(
        record.stake,
        scenario.missed_validator_stake.expect("baseline stake")
    );
    assert_eq!(
        record.slash_count,
        scenario
            .missed_validator_slash_count
            .expect("baseline slash count"),
        "TEE expiry must not record a felony slash"
    );
    drop(scenario);
    wait_until(
        || world.localnet.validator_exited(MISSED_VALIDATOR),
        60,
        "expired validator fail-stop",
    );

    let primary = world.validators.primary_port();
    let before = world
        .rpc
        .finalized(primary)
        .expect("finality before quorum proof");
    assert!(
        world.rpc.wait_finalized_at_least(primary, before + 3, 240),
        "remaining 3-of-4 quorum did not keep finalizing"
    );
    let height = world
        .rpc
        .finalized(primary)
        .expect("quorum finalized height");
    let expected_hash = world.rpc.block_hash(primary, height).expect("primary hash");
    for index in 0..3 {
        let port = world.validators.http_port(index);
        assert!(
            world.rpc.wait_finalized_at_least(port, height, 120),
            "validator {index} did not reach quorum height"
        );
        assert_eq!(
            world.rpc.block_hash(port, height),
            Some(expected_hash.clone())
        );
    }
}

#[then("the expired FullNode fail-stops and late renewal is rejected for both missed nodes")]
fn full_node_stops_and_late_renewal_fails(world: &mut World) {
    let full_node = state().full_node_index.expect("FullNode index");
    wait_until(
        || world.localnet.joiner_full_node_exited(full_node),
        120,
        "expired FullNode fail-stop",
    );
    for index in [MISSED_VALIDATOR, full_node] {
        let error = world
            .localnet
            .renew_node_enclave_expected_failure(index)
            .unwrap_or_else(|failure| panic!("late renewal node {index}: {failure:#}"));
        let error = error.to_ascii_lowercase();
        assert!(
            error.contains("expired") || error.contains("rejoin"),
            "late renewal did not require rejoin: {error}"
        );
    }
}

#[when("the jailed validator is excluded at the normal DKG boundary")]
fn wait_for_jail_exclusion_boundary(world: &mut World) {
    let address = state()
        .missed_validator_address
        .clone()
        .expect("missed validator address");
    wait_until(
        || {
            world
                .rpc
                .validator_record(world.validators.primary_port(), &address)
                .is_some_and(|record| record.status == STATUS_JAILED && !record.has_bls_share)
        },
        900,
        "normal boundary exclusion of the jailed validator",
    );
}

#[when("validator 3 unjails and both expired nodes complete fresh TEE join")]
fn expired_nodes_rejoin(world: &mut World) {
    let validator_key = world
        .validators
        .get(MISSED_VALIDATOR)
        .evm_key()
        .expect("missed validator key");
    let unjail = world
        .rpc
        .unjail_validator(&validator_key)
        .expect("submit ordinary unjail");
    assert!(unjail.success, "ordinary post-boundary unjail failed");
    let address = state()
        .missed_validator_address
        .clone()
        .expect("missed validator address");
    let record = world
        .rpc
        .validator_record(world.validators.primary_port(), &address)
        .expect("validator after unjail");
    assert_eq!(record.status, STATUS_PENDING, "unjail must produce PENDING");
    assert!(!record.has_bls_share, "unjail must remain shareless");

    let full_node = state().full_node_index.expect("FullNode index");
    let now = world
        .localnet
        .node_renewal_status(0)
        .expect("live validator finalized time")
        .finalized_timestamp;
    let next_deadline = now.checked_add(LEASE_SECONDS).expect("rejoin deadline");
    world
        .localnet
        .join_node_enclave_until(MISSED_VALIDATOR, next_deadline)
        .expect("expired validator tee join");
    world
        .localnet
        .join_node_enclave_until(full_node, next_deadline)
        .expect("expired FullNode tee join");

    world
        .localnet
        .restart_validator(MISSED_VALIDATOR)
        .expect("restart rejoined validator");
    world
        .localnet
        .launch_dcap_full_node("tee-lease-full-node", full_node, 0)
        .expect("restart rejoined FullNode");
}

#[then("validator 3 returns only after readiness and DKG while the FullNode resumes sync")]
fn recovered_nodes_resume(world: &mut World) {
    let address = state()
        .missed_validator_address
        .clone()
        .expect("missed validator address");
    let validator_key = world
        .validators
        .get(MISSED_VALIDATOR)
        .evm_key()
        .expect("missed validator key");
    assert_eq!(
        world
            .rpc
            .validator_record(world.validators.primary_port(), &address)
            .expect("validator before readiness")
            .status,
        STATUS_PENDING,
        "TEE join must not bypass readiness"
    );
    let ready = world
        .rpc
        .confirm_ready_outcome(&validator_key, MISSED_VALIDATOR)
        .expect("submit readiness after TEE rejoin");
    assert!(ready.success, "readiness confirmation failed");
    wait_until(
        || {
            world
                .rpc
                .validator_record(world.validators.primary_port(), &address)
                .is_some_and(|record| record.status == STATUS_ACTIVE && record.has_bls_share)
        },
        900,
        "fresh DKG activation after readiness",
    );

    let full_node = state().full_node_index.expect("FullNode index");
    let primary = world.validators.primary_port();
    let checkpoint = world.rpc.finalized(primary).expect("recovery checkpoint");
    wait_for_live_full_node_checkpoint(world, full_node, checkpoint, Duration::from_secs(240))
        .unwrap_or_else(|error| panic!("rejoined FullNode did not resume sync: {error}"));
    for index in [MISSED_VALIDATOR, full_node] {
        assert_eq!(
            world
                .localnet
                .node_offer_public(index)
                .unwrap_or_else(|error| panic!("recovered node {index} offer key: {error:#}")),
            state().permanent_offer_keys[index],
            "expired recovery changed node {index} permanent offer key"
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn full_node_sync_probe_fails_immediately_when_the_process_exits() {
        assert_eq!(
            super::classify_full_node_sync_probe(Some(6), 7, true),
            super::FullNodeSyncProbeV1::Exited
        );
    }

    #[test]
    fn full_node_sync_probe_requires_a_live_node_at_the_checkpoint() {
        assert_eq!(
            super::classify_full_node_sync_probe(Some(6), 7, false),
            super::FullNodeSyncProbeV1::Pending
        );
        assert_eq!(
            super::classify_full_node_sync_probe(Some(7), 7, false),
            super::FullNodeSyncProbeV1::Reached(7)
        );
        assert_eq!(
            super::classify_full_node_sync_probe(Some(9), 7, false),
            super::FullNodeSyncProbeV1::Reached(9)
        );
    }
}
