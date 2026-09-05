//! Release real-SGX/no-DCAP evidence for the recurring manual TEE lease.

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

const MISSED_VALIDATOR: usize = 3;
const STATUS_PENDING: u8 = 1;
const STATUS_ACTIVE: u8 = 2;
const STATUS_JAILED: u8 = 6;
const WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;
const LEASE_SECONDS: u64 = 14 * 24 * 60 * 60;
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

fn recovery_follower_has_post_start_progress(baseline: u64, current: Option<u64>) -> bool {
    current.is_some_and(|height| height > baseline)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryFollowerStartupProbeV1 {
    Ready,
    Waiting,
    Exited,
}

fn classify_recovery_follower_startup(
    running: bool,
    post_start_log: &str,
) -> RecoveryFollowerStartupProbeV1 {
    if !running {
        RecoveryFollowerStartupProbeV1::Exited
    } else if post_start_log.contains(FOLLOWER_ENGINE_STARTED_MARKER) {
        RecoveryFollowerStartupProbeV1::Ready
    } else {
        RecoveryFollowerStartupProbeV1::Waiting
    }
}

fn recovery_log_tail(log: &str, max_lines: usize) -> String {
    let mut lines: Vec<_> = log.lines().rev().take(max_lines).collect();
    lines.reverse();
    lines.join("\n")
}

fn wait_for_recovery_follower_startup(
    world: &mut World,
    follower_name: &str,
    log: &mut LeaseLogCapture,
    attempts: u32,
    label: &str,
) {
    for _ in 0..attempts {
        let post_start_log = log.read();
        let running = world.localnet.follower_running(follower_name);
        match classify_recovery_follower_startup(running, &post_start_log) {
            RecoveryFollowerStartupProbeV1::Ready => return,
            RecoveryFollowerStartupProbeV1::Waiting => sleep(Duration::from_secs(2)),
            RecoveryFollowerStartupProbeV1::Exited => panic!(
                "{label} exited before `{FOLLOWER_ENGINE_STARTED_MARKER}`; post-offset log tail:\n{}",
                recovery_log_tail(&post_start_log, 80)
            ),
        }
    }

    let post_start_log = log.read();
    let running = world.localnet.follower_running(follower_name);
    match classify_recovery_follower_startup(running, &post_start_log) {
        RecoveryFollowerStartupProbeV1::Ready => {}
        RecoveryFollowerStartupProbeV1::Exited => panic!(
            "{label} exited before `{FOLLOWER_ENGINE_STARTED_MARKER}`; post-offset log tail:\n{}",
            recovery_log_tail(&post_start_log, 80)
        ),
        RecoveryFollowerStartupProbeV1::Waiting => panic!(
            "timed out waiting for {label}; running={running}; post-offset log tail:\n{}",
            recovery_log_tail(&post_start_log, 80)
        ),
    }
}

fn requires_certified_follower_recovery(evidence: &str) -> bool {
    CERTIFIED_FOLLOWER_RECOVERY_MARKERS
        .iter()
        .all(|marker| evidence.contains(marker))
}

fn pre_readiness_recovery_is_authority_free(
    validator_status: u8,
    has_bls_share: bool,
    post_restart_log: &str,
) -> bool {
    validator_status == STATUS_PENDING
        && !has_bls_share
        && SHARELESS_MARKERS
            .iter()
            .all(|marker| has_info_message(post_restart_log, "outbe_engine::stack", marker))
        && AUTHORITY_MARKERS
            .iter()
            .all(|marker| !post_restart_log.contains(marker))
}

fn fresh_dkg_activation_is_complete(
    validator_status: u8,
    has_bls_share: bool,
    previous_material_version: u64,
    primary_material_version: u64,
    restarted_material_version: u64,
    post_restart_log: &str,
) -> bool {
    validator_status == STATUS_ACTIVE
        && has_bls_share
        && primary_material_version > previous_material_version
        && restarted_material_version == primary_material_version
        && activation_record(post_restart_log, restarted_material_version).is_some()
}

fn info_message<'a>(line: &'a str, target: &str) -> Option<&'a str> {
    let (prefix, message) = line.split_once(&format!("{target}: "))?;
    (prefix.split_whitespace().last() == Some("INFO")).then_some(message)
}

fn has_info_message(log: &str, target: &str, message: &str) -> bool {
    log.lines().any(|line| {
        info_message(line, target).is_some_and(|body| {
            body == message
                || body
                    .strip_prefix(message)
                    .is_some_and(|tail| tail.starts_with(' '))
        })
    })
}

fn log_u64(line: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    let mut values = line
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix));
    let value = values.next()?.parse().ok()?;
    values.next().is_none().then_some(value)
}

/// Completion must precede activation within the post-readiness process interval.
/// Public material alone never supplies the local-private-share assertion.
fn activation_record(log: &str, version: u64) -> Option<(u64, u64)> {
    let mut completed = false;
    for line in log.lines() {
        if info_message(line, "outbe_consensus::dkg_actor::actor") == Some(DKG_COMPLETE_MARKER) {
            completed = true;
        }
        if completed
            && info_message(line, "outbe_engine::stack")
                .is_some_and(|body| body.starts_with("VRF/DKG material activated "))
            && log_u64(line, "vrf_material_version") == Some(version)
        {
            return Some((
                log_u64(line, "dkg_cycle")?,
                log_u64(line, "activation_height")?,
            ));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalizedClockWaitDecisionV1 {
    Reached,
    Progressed,
    Waiting,
    Stalled,
    Regressed,
}

fn classify_finalized_clock_wait(
    current: (u64, u64),
    target_timestamp: u64,
    previous: (u64, u64),
    now: Instant,
    progress_deadline: Instant,
) -> FinalizedClockWaitDecisionV1 {
    if current.1 >= target_timestamp {
        FinalizedClockWaitDecisionV1::Reached
    } else if current.0 < previous.0 || current.1 < previous.1 {
        FinalizedClockWaitDecisionV1::Regressed
    } else if current.0 > previous.0 || current.1 > previous.1 {
        FinalizedClockWaitDecisionV1::Progressed
    } else if now >= progress_deadline {
        FinalizedClockWaitDecisionV1::Stalled
    } else {
        FinalizedClockWaitDecisionV1::Waiting
    }
}

fn finalized_clock_observation(world: &World, port: u16) -> Option<(u64, u64)> {
    let height = world.rpc.finalized(port)?;
    let timestamp = world.rpc.block_timestamp(port, height)?;
    Some((height, timestamp))
}

pub(super) fn wait_for_finalized_timestamp(
    world: &World,
    target_timestamp: u64,
    label: &str,
) -> (u64, u64) {
    let port = world.validators.primary_port();
    let initial_deadline = Instant::now() + FINALIZED_CLOCK_STALL_TIMEOUT;
    let mut previous = loop {
        if let Some(observation) = finalized_clock_observation(world, port) {
            break observation;
        }
        assert!(
            Instant::now() < initial_deadline,
            "timed out reading the initial finalized clock while waiting for {label}"
        );
        sleep(Duration::from_secs(1));
    };
    let started = previous;
    let max_step_seconds = outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS / 1_000;
    let minimum_remaining_blocks = target_timestamp
        .saturating_sub(previous.1)
        .div_ceil(max_step_seconds);
    eprintln!(
        "[tee-lease] waiting for {label}: start_height={}, start_timestamp={}, \
         target_timestamp={target_timestamp}, minimum_remaining_blocks={minimum_remaining_blocks}",
        started.0, started.1
    );

    let mut progress_deadline = Instant::now() + FINALIZED_CLOCK_STALL_TIMEOUT;
    loop {
        let now = Instant::now();
        let Some(current) = finalized_clock_observation(world, port) else {
            assert!(
                now < progress_deadline,
                "finalized RPC stalled while waiting for {label}: \
                 start={started:?}, last={previous:?}, target_timestamp={target_timestamp}"
            );
            sleep(Duration::from_secs(1));
            continue;
        };
        match classify_finalized_clock_wait(
            current,
            target_timestamp,
            previous,
            now,
            progress_deadline,
        ) {
            FinalizedClockWaitDecisionV1::Reached => {
                eprintln!(
                    "[tee-lease] reached {label}: height={}, timestamp={}, target_timestamp={target_timestamp}",
                    current.0, current.1
                );
                return current;
            }
            FinalizedClockWaitDecisionV1::Progressed => {
                previous = current;
                progress_deadline = now + FINALIZED_CLOCK_STALL_TIMEOUT;
            }
            FinalizedClockWaitDecisionV1::Waiting => {}
            FinalizedClockWaitDecisionV1::Stalled => {
                panic!(
                    "finalized clock stalled while waiting for {label}: \
                     start={started:?}, last={current:?}, target_timestamp={target_timestamp}"
                );
            }
            FinalizedClockWaitDecisionV1::Regressed => {
                panic!(
                    "finalized clock regressed while waiting for {label}: \
                     previous={previous:?}, current={current:?}, target_timestamp={target_timestamp}"
                );
            }
        }
        sleep(Duration::from_secs(1));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FullNodeSyncProbeV1 {
    NotTracked,
    Pending,
    Reached(u64),
    Exited,
}

fn classify_full_node_sync_probe(
    head: Option<u64>,
    checkpoint: u64,
    exited: Option<bool>,
) -> FullNodeSyncProbeV1 {
    if exited.is_none() {
        FullNodeSyncProbeV1::NotTracked
    } else if exited == Some(true) {
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
    let expected = world
        .rpc
        .checkpoint_at(world.validators.primary_port(), checkpoint)
        .map_err(|error| error.to_string())?;
    loop {
        let head = world.rpc.finalized(port);
        let exited = world.localnet.joiner_full_node_exit_status(full_node);
        match classify_full_node_sync_probe(head, checkpoint, exited) {
            FullNodeSyncProbeV1::Reached(height) => {
                let actual = world
                    .rpc
                    .checkpoint_at(port, checkpoint)
                    .map_err(|error| error.to_string())?;
                if actual != expected
                    || world.localnet.joiner_full_node_exit_status(full_node) != Some(false)
                {
                    return Err(
                        "FullNode finalized checkpoint differs or owned process exited".to_owned(),
                    );
                }
                world
                    .state
                    .tee_lease
                    .observations
                    .push(json!({"phase": "full_node_checkpoint",
                    "slot": full_node, "height": checkpoint, "block_hash": actual.block_hash,
                    "state_root": actual.state_root, "observed_finalized_height": height}));
                return Ok(height);
            }
            FullNodeSyncProbeV1::NotTracked => {
                return Err(format!(
                    "FullNode {full_node} has no owned process under canonical key {}; inspect {}",
                    crate::world::localnet::Localnet::joiner_full_node_name(full_node),
                    node_log.display()
                ));
            }
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

fn wait_for_live_validator_checkpoint(
    world: &mut World,
    validator: usize,
    checkpoint: u64,
    expected_hash: &str,
    timeout: Duration,
) -> Result<u64, String> {
    let port = world.validators.http_port(validator);
    let node_log = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{validator}/node.log"));
    let started = Instant::now();
    loop {
        if !world.localnet.validator_running(validator) {
            return Err(format!(
                "validator-{validator} exited before reaching finalized recovery checkpoint \
                 {checkpoint}; inspect {}",
                node_log.display()
            ));
        }
        if let Some(height) = world.rpc.finalized(port) {
            if height >= checkpoint {
                let observed_hash = world.rpc.block_hash(port, checkpoint).ok_or_else(|| {
                    format!(
                        "validator-{validator} finalized height {height} but cannot read block \
                         {checkpoint}; inspect {}",
                        node_log.display()
                    )
                })?;
                if observed_hash != expected_hash {
                    return Err(format!(
                        "validator-{validator} recovery checkpoint hash mismatch at height \
                         {checkpoint}: committee {expected_hash}, validator {observed_hash}; \
                         inspect {}",
                        node_log.display()
                    ));
                }
                return Ok(height);
            }
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "validator-{validator} remained alive but did not reach finalized recovery \
                 checkpoint {checkpoint} within {}s; inspect {}",
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
    world
        .localnet
        .stop_follower(&follower_name)
        .expect("interrupt validator recovery follower");
    let first_recovery_evidence = first_recovery_log.read();
    assert!(
        first_recovery_evidence
            .contains("local TEE lease guard armed at authenticated catch-up anchor"),
        "first recovery incarnation omitted authenticated admission anchor"
    );
    retain_log(world, "first_recovery_follower", first_recovery_log);

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
    // before submission. The second capture includes the submission boundary;
    // its prefix must overlap only authority-free pre-submission records.
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
        .confirm_ready_outcome(&validator_key, MISSED_VALIDATOR)
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
                if !fresh_dkg_activation_is_complete(
                    record.status,
                    record.has_bls_share,
                    previous_material_version,
                    primary_material.version,
                    local.version,
                    &log,
                ) || !local.has_threshold_shares
                    || activation_height <= readiness.height
                    || activation_height > height
                    || activation_height != local.last_activation_height
                {
                    return Ok(false);
                }
                let boundary = world.rpc.checkpoint_at(primary, activation_height)?;
                let boundary_record =
                    finalized_validator_at(world, "activation_boundary", &address, boundary)?;
                ensure!(
                    boundary_record.status == STATUS_ACTIVE && boundary_record.has_bls_share,
                    "activation event differs from canonical boundary membership"
                );
                world.state.tee_lease.observations.push(json!({"phase": "fresh_activation",
                "pid": state().validator_restart_pid, "dkg_cycle": cycle, "activation_height": activation_height,
                "activation_hash": boundary.block_hash, "material_version": local.version,
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
mod tests {
    // Exact production message/target/severity spellings. Every negative test
    // starts from this independently asserted affirmative fixture.
    fn shareless_fixture() -> String {
        "2026-09-05T05:06:12.322609Z INFO outbe_engine::stack: local validator is absent from the finalized DKG boundary; restoring shareless verifier mode dkg_output_hash=0x00\n\
         2026-09-05T05:06:12.323719Z INFO outbe_engine::stack: shareless verifier is not yet in the canonical reshare target; retaining no proposer identity epoch=7\n\
         2026-09-05T05:06:12.330995Z INFO outbe_engine::stack: no threshold share for this epoch - running consensus engine in VERIFIER mode epoch=7".to_owned()
    }

    fn activation_fixture() -> &'static str {
        "INFO outbe_consensus::dkg_actor::actor: DKG ceremony complete - threshold material obtained\n\
         INFO outbe_engine::stack: VRF/DKG material activated dkg_cycle=9 activation_height=540 vrf_material_version=9"
    }

    #[test]
    fn certified_follower_startup_probe_fails_closed_on_early_exit() {
        assert_eq!(
            super::classify_recovery_follower_startup(true, super::FOLLOWER_ENGINE_STARTED_MARKER),
            super::RecoveryFollowerStartupProbeV1::Ready
        );
        assert_eq!(
            super::classify_recovery_follower_startup(true, "still reconstructing committees"),
            super::RecoveryFollowerStartupProbeV1::Waiting
        );
        assert_eq!(
            super::classify_recovery_follower_startup(false, "fatal startup error"),
            super::RecoveryFollowerStartupProbeV1::Exited
        );
        assert_eq!(
            super::classify_recovery_follower_startup(false, super::FOLLOWER_ENGINE_STARTED_MARKER),
            super::RecoveryFollowerStartupProbeV1::Exited,
            "a dead follower must not satisfy the running-and-marker gate"
        );
    }

    #[test]
    fn certified_follower_recovery_evidence_requires_all_operator_guidance() {
        let complete = "validator recovery requires certified follower catch-up; \
            omit --validator and use --upstream <healthy-certified-rpc>";
        assert!(super::requires_certified_follower_recovery(complete));
        assert!(!super::requires_certified_follower_recovery(
            "validator recovery requires certified follower catch-up; omit --validator"
        ));
    }

    #[test]
    fn pre_readiness_recovery_requires_explicit_shareless_runtime_evidence() {
        let log = shareless_fixture();
        assert!(super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &log,
        ));
        assert!(!super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            true,
            &log,
        ));
    }

    #[test]
    fn pre_readiness_recovery_rejects_validator_authority_markers() {
        let baseline = shareless_fixture();
        assert!(super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &baseline
        ));
        for marker in super::AUTHORITY_MARKERS {
            assert!(
                !super::pre_readiness_recovery_is_authority_free(
                    super::STATUS_PENDING,
                    false,
                    &format!("{baseline}\nINFO outbe_engine::stack: {marker}")
                ),
                "missed {marker}"
            );
        }
    }

    #[test]
    fn fresh_dkg_activation_requires_share_version_advance_and_runtime_evidence() {
        let log = activation_fixture();
        assert!(super::fresh_dkg_activation_is_complete(
            super::STATUS_ACTIVE,
            true,
            7,
            9,
            9,
            log,
        ));
        assert!(!super::fresh_dkg_activation_is_complete(
            super::STATUS_ACTIVE,
            true,
            7,
            7,
            7,
            log,
        ));
    }

    #[test]
    fn recovery_follower_crash_requires_strict_post_start_progress() {
        assert!(!super::recovery_follower_has_post_start_progress(357, None));
        assert!(!super::recovery_follower_has_post_start_progress(
            357,
            Some(357)
        ));
        assert!(super::recovery_follower_has_post_start_progress(
            357,
            Some(358)
        ));
    }

    #[test]
    fn lease_shareless_proof_rejects_each_missing_or_spoofed_positive() {
        let baseline = shareless_fixture();
        assert!(super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &baseline
        ));
        for line in baseline.lines() {
            let missing = baseline
                .lines()
                .filter(|other| *other != line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!super::pre_readiness_recovery_is_authority_free(
                super::STATUS_PENDING,
                false,
                &missing
            ));
        }
        for changed in [
            baseline.replace("INFO", "DEBUG"),
            baseline.replace("outbe_engine::stack", "other"),
            baseline.replace("epoch - running", "epoch — running"),
        ] {
            assert!(!super::pre_readiness_recovery_is_authority_free(
                super::STATUS_PENDING,
                false,
                &changed
            ));
        }
        assert!(!super::pre_readiness_recovery_is_authority_free(
            super::STATUS_ACTIVE,
            false,
            &baseline
        ));
        let public = format!("{baseline}\nINFO outbe_engine::stack: VRF material active vrf_material_version=8\nINFO outbe_engine::stack: authenticated public DKG handoff");
        assert!(super::pre_readiness_recovery_is_authority_free(
            super::STATUS_PENDING,
            false,
            &public
        ));
    }

    #[test]
    fn lease_activation_requires_order_exact_target_version_and_private_state() {
        let baseline = activation_fixture();
        assert!(super::fresh_dkg_activation_is_complete(
            super::STATUS_ACTIVE,
            true,
            7,
            9,
            9,
            baseline
        ));
        for changed in [
            baseline.lines().rev().collect::<Vec<_>>().join("\n"),
            baseline.replace("complete - threshold", "complete — threshold"),
            baseline.replace("outbe_consensus::dkg_actor::actor", "outbe_engine::stack"),
            baseline.replace("INFO", "DEBUG"),
            baseline.replace("vrf_material_version=9", "vrf_material_version=8"),
            baseline.replace("activation_height=540", "activation_height=bad"),
            baseline.replace("dkg_cycle=9", "dkg_cycle=9 dkg_cycle=10"),
        ] {
            assert!(
                !super::fresh_dkg_activation_is_complete(
                    super::STATUS_ACTIVE,
                    true,
                    7,
                    9,
                    9,
                    &changed
                ),
                "{changed}"
            );
        }
        for (status, share, primary, local) in [
            (super::STATUS_PENDING, true, 9, 9),
            (super::STATUS_ACTIVE, false, 9, 9),
            (super::STATUS_ACTIVE, true, 9, 8),
        ] {
            assert!(!super::fresh_dkg_activation_is_complete(
                status, share, 7, primary, local, baseline
            ));
        }
        assert!(super::activation_record(
            "INFO outbe_engine::stack: VRF material active vrf_material_version=9",
            9
        )
        .is_none());
    }

    #[test]
    fn lease_local_material_requires_explicit_boolean_in_same_response() {
        use serde_json::json;
        let observation =
            super::local_material(json!({"vrfMaterialVersion": "9", "lastDkgActivationHeight": 540, "hasThresholdShares": false}))
                .unwrap();
        assert_eq!(observation.version, 9);
        assert!(!observation.has_threshold_shares);
        for missing in [
            json!({"vrfMaterialVersion": 9}),
            json!({"hasThresholdShares": true}),
            json!({"vrfMaterialVersion": 9, "hasThresholdShares": "false"}),
            json!({"vrfMaterialVersion": 9, "hasThresholdShares": false}),
        ] {
            assert!(super::local_material(missing).is_err());
        }
    }

    #[test]
    fn lease_material_comparison_rejects_rotation_boundary_and_local_share_disagreement() {
        let primary = super::LocalMaterialObservation {
            version: 9,
            last_activation_height: 540,
            has_threshold_shares: true,
        };
        let local = super::LocalMaterialObservation {
            has_threshold_shares: false,
            ..primary.clone()
        };
        assert!(super::material_observations_agree(
            &primary, &local, &primary, &local
        ));
        for changed in [
            super::LocalMaterialObservation {
                version: 10,
                ..local.clone()
            },
            super::LocalMaterialObservation {
                last_activation_height: 600,
                ..local.clone()
            },
            super::LocalMaterialObservation {
                has_threshold_shares: true,
                ..local.clone()
            },
        ] {
            assert!(!super::material_observations_agree(
                &primary, &local, &primary, &changed
            ));
        }
        let different_boundary = super::LocalMaterialObservation {
            last_activation_height: 600,
            ..local.clone()
        };
        assert!(!super::material_observations_agree(
            &primary,
            &different_boundary,
            &primary,
            &different_boundary
        ));
    }

    #[test]
    fn lease_log_intervals_exclude_replacements_and_reject_truncation() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        std::fs::write(&path, "earlier process\n").unwrap();
        let capture = super::LeaseLogCapture::arm(&path).unwrap();
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(writer, "current process").unwrap();
        let interval = capture.finish();
        writeln!(writer, "replacement process").unwrap();
        assert_eq!(interval.content, "current process\n");
        assert_eq!(interval.end - interval.start, interval.content.len() as u64);
        let mut capture = super::LeaseLogCapture::arm(&path).unwrap();
        std::fs::write(&path, "").unwrap();
        assert!(capture.log.read().is_err());
    }

    #[test]
    fn finalized_clock_wait_refreshes_only_on_monotonic_progress() {
        let now = std::time::Instant::now();
        let progress_deadline = now + std::time::Duration::from_secs(180);

        assert_eq!(
            super::classify_finalized_clock_wait((10, 200), 600, (9, 100), now, progress_deadline,),
            super::FinalizedClockWaitDecisionV1::Progressed
        );
        assert_eq!(
            super::classify_finalized_clock_wait((10, 200), 600, (10, 200), now, progress_deadline,),
            super::FinalizedClockWaitDecisionV1::Waiting
        );
    }

    #[test]
    fn finalized_clock_wait_reaches_target_and_rejects_stall_or_regression() {
        let now = std::time::Instant::now();

        assert_eq!(
            super::classify_finalized_clock_wait((11, 600), 600, (10, 200), now, now),
            super::FinalizedClockWaitDecisionV1::Reached
        );
        assert_eq!(
            super::classify_finalized_clock_wait((10, 599), 600, (10, 599), now, now),
            super::FinalizedClockWaitDecisionV1::Stalled
        );
        assert_eq!(
            super::classify_finalized_clock_wait((9, 201), 600, (10, 200), now, now),
            super::FinalizedClockWaitDecisionV1::Regressed
        );
        assert_eq!(
            super::classify_finalized_clock_wait((11, 199), 600, (10, 200), now, now),
            super::FinalizedClockWaitDecisionV1::Regressed
        );
    }

    #[test]
    fn full_node_sync_probe_fails_immediately_when_the_process_exits() {
        assert_eq!(
            super::classify_full_node_sync_probe(Some(6), 7, Some(true)),
            super::FullNodeSyncProbeV1::Exited
        );
    }

    #[test]
    fn full_node_sync_probe_never_treats_a_missing_handle_as_exit_evidence() {
        assert_eq!(
            super::classify_full_node_sync_probe(None, 7, None),
            super::FullNodeSyncProbeV1::NotTracked
        );
        assert_eq!(
            crate::world::localnet::Localnet::joiner_full_node_name(4),
            "joiner-full-node-4"
        );
    }

    #[test]
    fn full_node_sync_probe_requires_a_live_node_at_the_checkpoint() {
        assert_eq!(
            super::classify_full_node_sync_probe(Some(6), 7, Some(false)),
            super::FullNodeSyncProbeV1::Pending
        );
        assert_eq!(
            super::classify_full_node_sync_probe(Some(7), 7, Some(false)),
            super::FullNodeSyncProbeV1::Reached(7)
        );
        assert_eq!(
            super::classify_full_node_sync_probe(Some(9), 7, Some(false)),
            super::FullNodeSyncProbeV1::Reached(9)
        );
    }
}
