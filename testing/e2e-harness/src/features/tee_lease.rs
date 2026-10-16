//! Release real-SGX/no-DCAP evidence for the recurring manual TEE lease.

use outbe_primitives::time::SECONDS_PER_DAY;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use serde_json::{json, Value};

use crate::features::common::boot_localnet;
use crate::internal::eth;
use crate::internal::launch_log::LaunchLog;
use crate::world::rpc::{FinalizedCheckpoint, ValidatorRecord};
use crate::world::state::{TeeLeaseLogIntervalV1, TeeLeaseShutdownV1};
use crate::world::World;

mod recovery_log;
mod waits;

use recovery_log::{
    activation_record, fresh_dkg_activation_is_complete, pre_readiness_recovery_is_authority_free,
    recovery_follower_has_post_start_progress, recovery_log_tail,
    requires_certified_follower_recovery, validate_interrupted_recovery_log,
    wait_for_recovery_follower_startup, MaterialVersions,
};
#[cfg(test)]
use recovery_log::{classify_recovery_follower_startup, RecoveryFollowerStartupProbeV1};
pub(super) use waits::wait_for_finalized_timestamp;
#[cfg(test)]
use waits::{
    classify_finalized_clock_wait, classify_full_node_sync_probe, FinalizedClockWaitDecisionV1,
    FullNodeSyncProbeV1,
};
use waits::{wait_for_live_full_node_checkpoint, wait_for_live_validator_checkpoint};

const MISSED_VALIDATOR: usize = 3;
const STATUS_PENDING: u8 = 1;
const STATUS_ACTIVE: u8 = 2;
const STATUS_JAILED: u8 = 6;
const WINDOW_SECONDS: u64 = 7 * SECONDS_PER_DAY;
const LEASE_SECONDS: u64 = 14 * SECONDS_PER_DAY;
const FINALIZED_CLOCK_STALL_TIMEOUT: Duration = Duration::from_secs(180);
const VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT: Duration = Duration::from_secs(240);
const FOLLOWER_ENGINE_STARTED_MARKER: &str = "follower engine started; syncing from upstream";
const VERIFIER_MARKER: &str =
    "no threshold share for this epoch - running consensus engine in VERIFIER mode";
const DKG_COMPLETE_MARKER: &str = "DKG ceremony complete - threshold material obtained";
const SHARELESS_MARKERS: [&str; 3] = [
    "local validator is absent from the finalized DKG boundary; restoring shareless verifier mode",
    "shareless verifier is not yet in the canonical reshare target; retaining no proposer identity",
    VERIFIER_MARKER,
];
const AUTHORITY_MARKERS: [&str; 7] = [
    "propose requested",
    "relay forwarding proposed block",
    "sent DKG ack",
    "sent share to player",
    "recorded finalized DKG dealer log",
    DKG_COMPLETE_MARKER,
    "VRF/DKG material activated",
];
const CERTIFIED_FOLLOWER_RECOVERY_MARKERS: [&str; 3] = [
    "certified follower",
    "omit --validator",
    "--upstream <healthy-certified-rpc>",
];

#[derive(Default)]
struct LeaseScenarioState {
    full_node_index: Option<usize>,
    original_deadline: Option<u64>,
    renewed_deadline: Option<u64>,
    missed_validator_address: Option<String>,
    missed_validator_stake: Option<U256>,
    missed_validator_slash_count: Option<u64>,
    missed_validator_last_finalized: Option<u64>,
    missed_validator_epoch: Option<u64>,
    validator_restart_log: Option<LeaseLogCapture>,
    validator_restart_pid: Option<u32>,
    full_node_pid: Option<u32>,
    full_node_logs: Vec<LeaseLogCapture>,
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

struct LeaseLogCapture {
    log: LaunchLog,
    interval: TeeLeaseLogIntervalV1,
}

impl LeaseLogCapture {
    fn arm(path: &Path) -> Result<Self> {
        let log = LaunchLog::arm(path)?;
        let start = log.start_offset();
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            log,
            interval: TeeLeaseLogIntervalV1 {
                path: path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                start,
                end: start,
                content: String::new(),
            },
        })
    }

    fn node(world: &World, index: usize) -> Self {
        Self::arm(
            &world
                .localnet
                .scenario_dir()
                .join(format!("validator-{index}/node.log")),
        )
        .expect("arm exact process node log before launch")
    }

    fn read(&mut self) -> String {
        self.log
            .read()
            .expect("read original process log without replacement or truncation")
    }

    fn finish(mut self) -> TeeLeaseLogIntervalV1 {
        self.log
            .seal()
            .expect("seal process log before replacement");
        self.interval.content = self.read();
        self.interval.end = self.interval.start + self.interval.content.len() as u64;
        self.interval
    }
}

fn retain_log(world: &mut World, phase: &str, capture: LeaseLogCapture) {
    world
        .state
        .tee_lease
        .observations
        .push(json!({"phase": phase, "log": capture.finish()}));
}

fn finalized_validator_at(
    world: &mut World,
    phase: &str,
    address: &str,
    checkpoint: FinalizedCheckpoint,
) -> Result<ValidatorRecord> {
    let primary = world.validators.primary_port();
    ensure!(
        world.rpc.finalized_result(primary)? >= checkpoint.height,
        "lease snapshot is not finalized"
    );
    let record = world
        .rpc
        .validator_record_at(primary, address, checkpoint.height)
        .ok_or_else(|| {
            eyre!(
                "missing finalized validator record at {}",
                checkpoint.height
            )
        })?;
    ensure!(
        world.rpc.checkpoint_at(primary, checkpoint.height)? == checkpoint,
        "lease snapshot canonical commitment changed"
    );
    world.state.tee_lease.observations.push(json!({
        "phase": phase, "height": checkpoint.height, "block_hash": checkpoint.block_hash,
        "state_root": checkpoint.state_root, "address": record.address, "status": record.status,
        "has_bls_share": record.has_bls_share, "stake": record.stake.to_string(),
        "slash_count": record.slash_count, "missed_blocks": record.missed_blocks,
        "missed_votes": record.missed_votes,
    }));
    Ok(record)
}

fn finalized_validator(world: &mut World, phase: &str, address: &str) -> Result<ValidatorRecord> {
    let port = world.validators.primary_port();
    let checkpoint = world
        .rpc
        .checkpoint_at(port, world.rpc.finalized_result(port)?)?;
    finalized_validator_at(world, phase, address, checkpoint)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct LocalMaterialObservation {
    version: u64,
    last_activation_height: u64,
    has_threshold_shares: bool,
}

fn local_material(value: Value) -> Result<LocalMaterialObservation> {
    let version = value
        .get("vrfMaterialVersion")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .ok_or_else(|| eyre!("consensus response omitted material version"))?;
    let has_threshold_shares = value
        .get("hasThresholdShares")
        .and_then(Value::as_bool)
        .ok_or_else(|| eyre!("consensus response omitted local share usability"))?;
    let last_activation_height = value
        .get("lastDkgActivationHeight")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .ok_or_else(|| eyre!("consensus response omitted activation boundary"))?;
    Ok(LocalMaterialObservation {
        version,
        last_activation_height,
        has_threshold_shares,
    })
}

fn coherent_material(
    world: &mut World,
    phase: &str,
) -> Result<(LocalMaterialObservation, LocalMaterialObservation)> {
    let primary = world.validators.primary_port();
    let restarted = world.validators.http_port(MISSED_VALIDATOR);
    let read = |port| -> Result<_> {
        local_material(eth::raw_json_result(
            &world.rpc.url(port),
            "outbe_consensusStatus",
            json!([]),
        )?)
    };
    let first_primary = read(primary)?;
    let first_local = read(restarted)?;
    let second_primary = read(primary)?;
    let second_local = read(restarted)?;
    world
        .state
        .tee_lease
        .observations
        .push(json!({"phase": phase,
        "primary_before": first_primary, "local_before": first_local,
        "primary_after": second_primary, "local_after": second_local}));
    ensure!(
        material_observations_agree(&first_primary, &first_local, &second_primary, &second_local),
        "material comparison crossed a rotation or nodes disagree"
    );
    Ok((first_primary, first_local))
}

fn material_observations_agree(
    primary: &LocalMaterialObservation,
    local: &LocalMaterialObservation,
    primary_after: &LocalMaterialObservation,
    local_after: &LocalMaterialObservation,
) -> bool {
    primary == primary_after
        && local == local_after
        && primary.version == local.version
        && primary.last_activation_height == local.last_activation_height
}

fn finalized_voter_misses(world: &mut World, phase: &str, address: &str) -> u64 {
    let port = world.validators.primary_port();
    let height = world
        .rpc
        .finalized_result(port)
        .expect("finalized miss-counter height");
    let checkpoint = world
        .rpc
        .checkpoint_at(port, height)
        .expect("miss-counter checkpoint");
    let count = eth::read_call_at_result(
        &world.rpc.url(port),
        crate::internal::addresses::SLASH_ADDR,
        &eth::ISlashIndicator::getVoterMissCountCall {
            validator: address.parse().expect("validator address"),
        },
        height,
    )
    .expect("finalized voter-miss counter");
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, height)
            .expect("recheck miss-counter checkpoint"),
        checkpoint
    );
    world
        .state
        .tee_lease
        .observations
        .push(json!({"phase": phase, "height": height,
        "block_hash": checkpoint.block_hash, "voter_miss_count": count}));
    count
}

fn ensure_recovered_process(world: &mut World) {
    assert!(
        world.localnet.validator_running(MISSED_VALIDATOR),
        "recovered validator exited"
    );
    assert_eq!(
        Some(
            world
                .localnet
                .validator_pid(MISSED_VALIDATOR)
                .expect("owned validator PID")
        ),
        state().validator_restart_pid,
        "recovered validator was replaced during the proof"
    );
}

fn record_lease_error(world: &mut World, phase: &str, error: &eyre::Report) {
    world
        .state
        .tee_lease
        .observations
        .push(json!({"phase": phase, "error": error.to_string()}));
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
    let record = finalized_validator(world, "baseline", &missed_address)
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
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("public chain id");
    let debug_dir = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{full_node}/logs/{chain_id}"));
    std::fs::create_dir_all(&debug_dir).expect("prepare FullNode DEBUG log capture");
    let logs = vec![
        LeaseLogCapture::node(world, full_node),
        LeaseLogCapture::arm(&debug_dir.join("reth.log")).expect("arm FullNode DEBUG sink"),
    ];
    world
        .localnet
        .launch_dcap_full_node(
            &crate::world::localnet::Localnet::joiner_full_node_name(full_node),
            full_node,
            0,
        )
        .expect("launch role-neutral FullNode");
    let (pid, exit) = world
        .localnet
        .owned_full_node_process(full_node)
        .expect("owned FullNode launch");
    assert!(exit.is_none(), "FullNode exited during launch");
    state().full_node_pid = Some(pid);
    state().full_node_logs = logs;

    let checkpoint = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("primary finalized checkpoint before FullNode lease testing");
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
    wait_for_finalized_timestamp(world, opens_at, "the manual renewal window");
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
    let mut scenario = state();
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
    for (index, expected_offer_key) in scenario.permanent_offer_keys[..=full_node]
        .iter()
        .enumerate()
    {
        assert_eq!(
            world
                .localnet
                .node_offer_public(index)
                .unwrap_or_else(|error| panic!("read node {index} offer key: {error:#}")),
            *expected_offer_key,
            "manual lease transition changed node {index} permanent offer key"
        );
    }
    scenario.missed_validator_last_finalized = world
        .rpc
        .finalized(world.validators.http_port(MISSED_VALIDATOR));
    scenario.missed_validator_epoch = world
        .rpc
        .epoch_on(world.validators.http_port(MISSED_VALIDATOR));
}

#[when("finalized consensus time reaches the original lease deadline")]
fn reach_original_deadline(world: &mut World) {
    let deadline = state().original_deadline.expect("original deadline");
    world
        .localnet
        .restart_committee_at_consensus_timestamp(deadline.saturating_sub(120))
        .expect("restart committee immediately before lease deadline");
    wait_for_finalized_timestamp(world, deadline, "the original lease deadline");
    wait_until(
        || {
            let address = state()
                .missed_validator_address
                .clone()
                .expect("missed validator address");
            finalized_validator(world, "await_jail", &address)
                .is_ok_and(|record| record.status == STATUS_JAILED)
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
    let record = finalized_validator(world, "jailed_without_slash", address)
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
    world.state.expected_tee_lease_guard_shutdown_validator = Some(MISSED_VALIDATOR);

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
        || {
            world
                .localnet
                .joiner_full_node_exit_status(full_node)
                .expect("lease FullNode must remain owned until fail-stop")
        },
        120,
        "expired FullNode fail-stop",
    );
    let (pid, exit) = world
        .localnet
        .owned_full_node_process(full_node)
        .expect("observe actual original FullNode exit");
    let exit = exit.expect("FullNode exit must be observed before replacement");
    let mut scenario = state();
    assert_eq!(
        Some(pid),
        scenario.full_node_pid,
        "FullNode process was replaced before expiry"
    );
    let proof = TeeLeaseShutdownV1 {
        slot: full_node,
        pid,
        deadline: scenario.original_deadline.expect("lease deadline"),
        exit_code: exit.code(),
        exit_signal: exit.signal(),
        sinks: std::mem::take(&mut scenario.full_node_logs)
            .into_iter()
            .map(LeaseLogCapture::finish)
            .collect(),
    };
    drop(scenario);
    world.state.tee_lease.full_node_shutdown = Some(proof.clone());
    world.state.expected_tee_lease_guard_shutdown_full_node = Some(proof);
    assert!(
        exit.success(),
        "lease FullNode did not exit through controlled shutdown: {exit}"
    );
    // Preserve the original exit and sealed log proof above before releasing
    // this exited owner. Recovery must never bypass a live-datadir guard.
    world
        .localnet
        .stop_follower(&crate::world::localnet::Localnet::joiner_full_node_name(
            full_node,
        ))
        .expect("reap the proven exited lease FullNode before later admission");
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
            finalized_validator(world, "await_exclusion", &address)
                .is_ok_and(|record| record.status == STATUS_JAILED && !record.has_bls_share)
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
    let unjail_checkpoint = world
        .rpc
        .finalize_outcome(&unjail, &[world.validators.primary_port()], 60)
        .expect("canonical finalized unjail receipt");
    let address = state()
        .missed_validator_address
        .clone()
        .expect("missed validator address");
    let record = finalized_validator_at(world, "unjail_receipt", &address, unjail_checkpoint)
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
        .launch_dcap_full_node(
            &crate::world::localnet::Localnet::joiner_full_node_name(full_node),
            full_node,
            0,
        )
        .expect("restart rejoined FullNode");
}

#[when("stale validator 3 fails closed and catches up through its certified follower datadir")]
fn stale_validator_recovers_through_certified_follower(world: &mut World) {
    let primary = world.validators.primary_port();
    let stale_epoch = state()
        .missed_validator_epoch
        .expect("captured validator epoch before expiry");

    let mut startup_log = LeaseLogCapture::node(world, MISSED_VALIDATOR);
    let immediate_startup_error = world
        .localnet
        .restart_validator(MISSED_VALIDATOR)
        .err()
        .map(|error| error.to_string());
    if immediate_startup_error.is_none() {
        wait_until(
            || {
                !world.localnet.validator_running(MISSED_VALIDATOR)
                    && requires_certified_follower_recovery(&startup_log.read())
            },
            60,
            "stale validator certified-follower fail-closed verdict",
        );
    }
    let startup_evidence = format!(
        "{}\n{}",
        immediate_startup_error.unwrap_or_default(),
        startup_log.read()
    );
    assert!(
        requires_certified_follower_recovery(&startup_evidence),
        "stale validator startup omitted certified-follower recovery guidance: {startup_evidence}"
    );
    assert!(
        !world.localnet.validator_running(MISSED_VALIDATOR),
        "stale validator retained authority after fail-closed startup"
    );

    retain_log(world, "stale_validator_rejection", startup_log);
    let mut first_recovery_log = LeaseLogCapture::node(world, MISSED_VALIDATOR);
    let follower_name = world
        .localnet
        .launch_validator_recovery_follower(MISSED_VALIDATOR, 0)
        .expect("launch validator datadir as certified follower");
    assert!(
        !world
            .localnet
            .validator_radicle_sidecar_running(MISSED_VALIDATOR),
        "validator Radicle signer remained live during certified follower recovery"
    );
    wait_for_recovery_follower_startup(
        world,
        &follower_name,
        &mut first_recovery_log,
        120,
        "first certified recovery follower engine startup",
    );
    let recovery_port = world.validators.http_port(MISSED_VALIDATOR);
    let first_run_baseline = world
        .rpc
        .finalized(recovery_port)
        .expect("first recovery follower finalized baseline after engine startup");
    wait_until(
        || {
            world.localnet.follower_running(&follower_name)
                && recovery_follower_has_post_start_progress(
                    first_run_baseline,
                    world.rpc.finalized(recovery_port),
                )
        },
        120,
        "first certified recovery follower post-start finalized progress",
    );
    let interrupted_height = world
        .rpc
        .finalized_result(recovery_port)
        .expect("read finalized progress before interrupting recovery");
    assert!(interrupted_height > first_run_baseline);
    let interrupted_checkpoint = world
        .rpc
        .checkpoint_at(recovery_port, interrupted_height)
        .expect("read exact checkpoint before interrupting recovery");
    assert!(
        world
            .rpc
            .finalized_result(primary)
            .expect("primary finality")
            >= interrupted_height
    );
    assert_eq!(
        interrupted_checkpoint,
        world
            .rpc
            .checkpoint_at(primary, interrupted_height)
            .expect("canonical interrupted checkpoint"),
        "first recovery follower made noncanonical progress"
    );
    world
        .localnet
        .stop_follower(&follower_name)
        .expect("interrupt validator recovery follower");
    let first_recovery_evidence = first_recovery_log.read();
    retain_log(world, "first_recovery_follower", first_recovery_log);
    validate_interrupted_recovery_log(&first_recovery_evidence)
        .expect("first recovery incarnation must be interrupted before admission");

    let mut recovery_log_capture = LeaseLogCapture::node(world, MISSED_VALIDATOR);
    let restarted_name = world
        .localnet
        .launch_validator_recovery_follower(MISSED_VALIDATOR, 0)
        .expect("restart validator recovery follower on the same datadir");
    assert_eq!(restarted_name, follower_name);
    wait_for_recovery_follower_startup(
        world,
        &restarted_name,
        &mut recovery_log_capture,
        120,
        "restarted certified recovery follower engine startup",
    );
    assert!(
        world
            .rpc
            .finalized_result(recovery_port)
            .expect("restarted follower finalized height")
            >= interrupted_height,
        "restarted follower lost finalized progress from its first incarnation"
    );
    assert_eq!(
        world
            .rpc
            .checkpoint_at(recovery_port, interrupted_height)
            .expect("restarted follower checkpoint"),
        interrupted_checkpoint,
        "restarted follower changed its previously finalized height/hash/root"
    );
    world.state.tee_lease.observations.push(json!({
        "phase": "interrupted_recovery_checkpoint_preserved",
        "baseline_height": first_run_baseline,
        "height": interrupted_checkpoint.height,
        "block_hash": interrupted_checkpoint.block_hash,
        "state_root": interrupted_checkpoint.state_root,
    }));
    let recovery_checkpoint = world
        .rpc
        .finalized(primary)
        .expect("sample healthy finalized recovery target");
    let recovery_checkpoint_hash = world
        .rpc
        .block_hash(primary, recovery_checkpoint)
        .expect("sample healthy finalized recovery target hash");
    wait_until(
        || {
            if !world.localnet.follower_running(&restarted_name) {
                return false;
            }
            let port = world.validators.http_port(MISSED_VALIDATOR);
            world.rpc.finalized(port).is_some_and(|height| {
                height >= recovery_checkpoint
                    && world.rpc.block_hash(port, recovery_checkpoint)
                        == Some(recovery_checkpoint_hash.clone())
            })
        },
        180,
        "certified follower exact canonical recovery checkpoint",
    );
    let recovered_epoch = world
        .rpc
        .epoch_on(world.validators.http_port(MISSED_VALIDATOR))
        .expect("recovered follower epoch");
    assert!(
        recovered_epoch > stale_epoch,
        "recovery follower did not cross the exclusion/DKG epoch boundary: stale {stale_epoch}, recovered {recovered_epoch}"
    );
    wait_until(
        || {
            recovery_log_capture
                .read()
                .contains("local TEE lease guard armed at authenticated catch-up anchor")
        },
        60,
        "authenticated certified follower admission anchor",
    );
    world
        .localnet
        .stop_follower(&restarted_name)
        .expect("stop recovered certified follower before validator restart");

    let recovery_log = format!(
        "{}\n{}",
        first_recovery_evidence,
        recovery_log_capture.read()
    );
    for forbidden in [
        "loaded validator EVM signer",
        "propose requested",
        "relay forwarding proposed block",
        "restoring durable DKG dealer transcript",
        "restoring durable dealer-only DKG transcript",
        "local validator is DKG player-only for this reshare",
        "sent DKG ack",
        "sent share to player",
        "recorded finalized DKG dealer log",
        DKG_COMPLETE_MARKER,
        "VRF material active",
    ] {
        assert!(
            !recovery_log.contains(forbidden),
            "certified follower recovery exercised validator authority marker `{forbidden}`"
        );
    }

    retain_log(world, "restarted_recovery_follower", recovery_log_capture);
    state().validator_restart_log = Some(LeaseLogCapture::node(world, MISSED_VALIDATOR));
    world
        .localnet
        .restart_validator(MISSED_VALIDATOR)
        .expect("restart caught-up validator in its original role");
    state().validator_restart_pid = Some(
        world
            .localnet
            .validator_pid(MISSED_VALIDATOR)
            .expect("owned recovered validator PID"),
    );
    wait_for_live_validator_checkpoint(
        world,
        MISSED_VALIDATOR,
        recovery_checkpoint,
        &recovery_checkpoint_hash,
        VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT,
    )
    .unwrap_or_else(|error| {
        panic!("rejoined validator did not catch up before readiness: {error}")
    });
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
        .expect("missed validator signing key");
    let primary = world.validators.primary_port();
    let mut pre_log = state()
        .validator_restart_log
        .take()
        .expect("owned restart log");
    wait_until(
        || {
            ensure_recovered_process(world);
            let log = pre_log.read();
            // Keep the last public predicate inputs even when this step times out.
            world
                .state
                .tee_lease
                .observations
                .push(json!({"phase": "await_shareless",
            "log_tail": recovery_log_tail(&log, 80)}));
            match finalized_validator(world, "await_shareless", &address) {
                Ok(record) => pre_readiness_recovery_is_authority_free(
                    record.status,
                    record.has_bls_share,
                    &log,
                ),
                Err(error) => {
                    record_lease_error(world, "await_shareless", &error);
                    false
                }
            }
        },
        60,
        "explicit shareless validator runtime before readiness",
    );

    let record =
        finalized_validator(world, "before_readiness", &address).expect("finalized PENDING record");
    let (primary_material, local) = coherent_material(world, "before_readiness_material")
        .expect("coherent pre-readiness material observations");
    assert!(
        !local.has_threshold_shares,
        "shareless validator reported usable private threshold shares"
    );
    let previous_material_version = primary_material.version;
    let voter_misses_before_readiness =
        finalized_voter_misses(world, "misses_before_readiness", &address);
    ensure_recovered_process(world);
    // Recheck after all RPC observations, then freeze this phase immediately
    // before submission. The second capture includes the submission boundary.
    // Its prefix must overlap only authority-free pre-submission records.
    let mut post_log = LeaseLogCapture::node(world, MISSED_VALIDATOR);
    let pre_interval = pre_log.finish();
    let no_authority = pre_readiness_recovery_is_authority_free(
        record.status,
        record.has_bls_share,
        &pre_interval.content,
    );
    world
        .state
        .tee_lease
        .observations
        .push(json!({"phase": "pre_readiness_interval", "log": pre_interval}));
    assert!(
        no_authority,
        "TEE join bypassed the shareless PENDING recovery state"
    );
    let ready = world
        .rpc
        .confirm_ready_outcome(&validator_key)
        .expect("submit readiness after TEE rejoin");
    assert!(ready.success, "readiness confirmation failed");
    let readiness = world
        .rpc
        .finalize_outcome(&ready, &[primary], 60)
        .expect("exact successful readiness receipt reached canonical finality");
    let readiness_record = finalized_validator_at(world, "readiness_receipt", &address, readiness)
        .expect("finalized readiness receipt record");
    assert!(
        readiness_record.status == STATUS_PENDING && !readiness_record.has_bls_share,
        "readiness bypassed fresh DKG activation at height {}",
        readiness.height
    );

    wait_until(
        || {
            ensure_recovered_process(world);
            let log = post_log.read();
            world
                .state
                .tee_lease
                .observations
                .push(json!({"phase": "await_activation",
            "log_tail": recovery_log_tail(&log, 80)}));
            let result = (|| -> Result<bool> {
                let height = world.rpc.finalized_result(primary)?;
                let checkpoint = world.rpc.checkpoint_at(primary, height)?;
                let record =
                    finalized_validator_at(world, "await_activation", &address, checkpoint)?;
                let (primary_material, local) =
                    coherent_material(world, "await_activation_material")?;
                let Some((cycle, activation_height)) = activation_record(&log, local.version)
                else {
                    return Ok(false);
                };
                let membership_height = activation_height
                    .checked_add(1)
                    .ok_or_else(|| eyre!("activation height overflow"))?;
                let versions = MaterialVersions {
                    previous: previous_material_version,
                    primary: primary_material.version,
                    restarted: local.version,
                };
                let fresh = fresh_dkg_activation_is_complete(
                    record.status,
                    record.has_bls_share,
                    versions,
                    &log,
                );
                let local_bound =
                    local.has_threshold_shares && activation_height == local.last_activation_height;
                let in_window = activation_height > readiness.height && membership_height <= height;
                if !(fresh && local_bound && in_window) {
                    return Ok(false);
                }
                let boundary = world.rpc.checkpoint_at(primary, membership_height)?;
                let boundary_record =
                    finalized_validator_at(world, "activation_boundary", &address, boundary)?;
                ensure!(
                    boundary_record.status == STATUS_ACTIVE && boundary_record.has_bls_share,
                    "activation event differs from canonical boundary membership"
                );
                world.state.tee_lease.observations.push(json!({"phase": "fresh_activation",
                "pid": state().validator_restart_pid, "dkg_cycle": cycle, "activation_height": activation_height,
                "membership_height": membership_height, "membership_hash": boundary.block_hash,
                "material_version": local.version,
                "local_has_threshold_shares": local.has_threshold_shares}));
                Ok(true)
            })();
            match result {
                Ok(complete) => complete,
                Err(error) => {
                    record_lease_error(world, "await_activation", &error);
                    false
                }
            }
        },
        900,
        "fresh DKG completion and activation after readiness",
    );
    // Keep the first accepted fresh ceremony separate from later rotations.
    retain_log(world, "post_readiness_activation_interval", post_log);

    let canonical = world
        .rpc
        .checkpoint_at(
            primary,
            world
                .rpc
                .finalized_result(primary)
                .expect("post-activation finalized height"),
        )
        .expect("post-activation checkpoint");
    wait_for_live_validator_checkpoint(
        world,
        MISSED_VALIDATOR,
        canonical.height,
        &format!("{:#x}", canonical.block_hash),
        VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("rejoined validator lost canonical parity: {error}"));
    ensure_recovered_process(world);
    let (active_primary, active_local) =
        coherent_material(world, "active_material").expect("coherent active material");
    assert!(active_local.has_threshold_shares && active_local.version > previous_material_version);
    let active_record = finalized_validator(world, "active_validator", &address)
        .expect("finalized active validator");
    assert!(active_record.status == STATUS_ACTIVE && active_record.has_bls_share);
    assert_eq!(active_local.version, active_primary.version);
    let misses = finalized_voter_misses(world, "misses_after_activation", &address);
    assert!(
        misses <= voter_misses_before_readiness,
        "validator accumulated voter misses through activation"
    );

    let full_node = state().full_node_index.expect("FullNode index");
    wait_for_live_full_node_checkpoint(
        world,
        full_node,
        canonical.height,
        Duration::from_secs(240),
    )
    .unwrap_or_else(|error| panic!("rejoined FullNode did not resume canonical sync: {error}"));
    for index in [MISSED_VALIDATOR, full_node] {
        let actual = world
            .localnet
            .node_offer_public(index)
            .expect("authenticated recovered offer public key");
        let expected = state().permanent_offer_keys[index];
        world
            .state
            .tee_lease
            .observations
            .push(json!({"phase": "permanent_offer_key",
            "slot": index, "expected": alloy_primitives::hex::encode(expected),
            "actual": alloy_primitives::hex::encode(actual)}));
        assert_eq!(
            actual, expected,
            "expired recovery changed node {index} permanent offer key"
        );
    }

    let completion = world
        .rpc
        .checkpoint_at(
            primary,
            world
                .rpc
                .finalized_result(primary)
                .expect("completion finalized height"),
        )
        .expect("completion checkpoint");
    wait_for_live_validator_checkpoint(
        world,
        MISSED_VALIDATOR,
        completion.height,
        &format!("{:#x}", completion.block_hash),
        VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("validator lost canonical parity before completion: {error}"));
    wait_for_live_full_node_checkpoint(
        world,
        full_node,
        completion.height,
        Duration::from_secs(240),
    )
    .unwrap_or_else(|error| panic!("FullNode lost canonical parity before completion: {error}"));
    ensure_recovered_process(world);
    let (_, completion_local) =
        coherent_material(world, "completion_material").expect("coherent completion material");
    assert!(
        completion_local.has_threshold_shares,
        "validator lost usable private threshold shares"
    );
    let completed = finalized_validator(world, "completion_validator", &address)
        .expect("completion finalized record");
    assert!(completed.status == STATUS_ACTIVE && completed.has_bls_share);
    assert_eq!(
        Some(completed.stake),
        state().missed_validator_stake,
        "recovery changed stake"
    );
    assert_eq!(
        Some(completed.slash_count),
        state().missed_validator_slash_count,
        "recovery introduced a slash"
    );
    let misses = finalized_voter_misses(world, "misses_at_completion", &address);
    assert!(
        misses <= voter_misses_before_readiness,
        "validator accumulated voter misses after activation"
    );
}

#[cfg(test)]
mod tests;
