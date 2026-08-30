//! Release real-SGX/no-DCAP evidence for the recurring manual TEE lease.

use std::io::{Read, Seek, SeekFrom};
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
const FINALIZED_CLOCK_STALL_TIMEOUT: Duration = Duration::from_secs(180);
const VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT: Duration = Duration::from_secs(240);
const FOLLOWER_ENGINE_STARTED_MARKER: &str = "follower engine started; syncing from upstream";

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

fn wait_for_finalized_timestamp(world: &World, target_timestamp: u64, label: &str) -> (u64, u64) {
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
    loop {
        let head = world.rpc.head(port);
        let exited = world.localnet.joiner_full_node_exit_status(full_node);
        match classify_full_node_sync_probe(head, checkpoint, exited) {
            FullNodeSyncProbeV1::Reached(height) => return Ok(height),
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

fn node_log_len(world: &World, index: usize) -> u64 {
    std::fs::metadata(
        world
            .localnet
            .scenario_dir()
            .join(format!("validator-{index}/node.log")),
    )
    .map(|metadata| metadata.len())
    .unwrap_or_default()
}

fn node_log_since(world: &World, index: usize, offset: u64) -> String {
    let path = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{index}/node.log"));
    let mut file = std::fs::File::open(&path)
        .unwrap_or_else(|error| panic!("open recovery log {}: {error}", path.display()));
    file.seek(SeekFrom::Start(offset))
        .unwrap_or_else(|error| panic!("seek recovery log {}: {error}", path.display()));
    let mut suffix = String::new();
    file.read_to_string(&mut suffix)
        .unwrap_or_else(|error| panic!("read recovery log {}: {error}", path.display()));
    suffix
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
        .launch_dcap_full_node(
            &crate::world::localnet::Localnet::joiner_full_node_name(full_node),
            full_node,
            0,
        )
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
        || {
            world
                .localnet
                .joiner_full_node_exit_status(full_node)
                .expect("lease FullNode must remain owned until fail-stop")
        },
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

    let startup_error = world
        .localnet
        .restart_validator(MISSED_VALIDATOR)
        .expect_err("stale validator startup must require certified follower recovery");
    let startup_error = startup_error.to_string();
    assert!(startup_error.contains("certified follower"));
    assert!(startup_error.contains("omit --validator"));
    assert!(startup_error.contains("--upstream <healthy-certified-rpc>"));
    assert!(
        !world.localnet.validator_running(MISSED_VALIDATOR),
        "stale validator retained authority after fail-closed startup"
    );

    let recovery_log_offset = node_log_len(world, MISSED_VALIDATOR);
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
    wait_until(
        || {
            world.localnet.follower_running(&follower_name)
                && node_log_since(world, MISSED_VALIDATOR, recovery_log_offset)
                    .contains(FOLLOWER_ENGINE_STARTED_MARKER)
        },
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

    let restarted_name = world
        .localnet
        .launch_validator_recovery_follower(MISSED_VALIDATOR, 0)
        .expect("restart validator recovery follower on the same datadir");
    assert_eq!(restarted_name, follower_name);
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
            node_log_since(world, MISSED_VALIDATOR, recovery_log_offset)
                .contains("local TEE lease guard armed at authenticated catch-up anchor")
        },
        60,
        "authenticated certified follower admission anchor",
    );
    world
        .localnet
        .stop_follower(&restarted_name)
        .expect("stop recovered certified follower before validator restart");

    let recovery_log = node_log_since(world, MISSED_VALIDATOR, recovery_log_offset);
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
        "DKG ceremony complete — threshold material obtained",
        "VRF material active",
    ] {
        assert!(
            !recovery_log.contains(forbidden),
            "certified follower recovery exercised validator authority marker `{forbidden}`"
        );
    }

    world
        .localnet
        .restart_validator(MISSED_VALIDATOR)
        .expect("restart caught-up validator in its original role");
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
        .expect("missed validator key");
    let restarted_port = world.validators.http_port(MISSED_VALIDATOR);
    assert_eq!(
        world
            .rpc
            .validator_record(world.validators.primary_port(), &address)
            .expect("validator before readiness")
            .status,
        STATUS_PENDING,
        "TEE join must not bypass readiness"
    );
    let voter_misses_before_readiness = world
        .rpc
        .voter_miss_count(world.validators.primary_port(), &address)
        .expect("restarted validator voter misses before readiness");
    assert_eq!(
        world.rpc.has_threshold_shares(restarted_port),
        Some(false),
        "PENDING validator exposed current threshold material before readiness"
    );
    let ready = world
        .rpc
        .confirm_ready_outcome(&validator_key, MISSED_VALIDATOR)
        .expect("submit readiness after TEE rejoin");
    assert!(ready.success, "readiness confirmation failed");
    let readiness_height = ready
        .block_number()
        .expect("readiness receipt block number");
    let primary = world.validators.primary_port();
    wait_until(
        || {
            world
                .rpc
                .finalized(primary)
                .is_some_and(|height| height >= readiness_height)
        },
        60,
        "readiness receipt canonical finality",
    );
    assert_eq!(
        world
            .rpc
            .validator_record(world.validators.primary_port(), &address)
            .expect("validator immediately after readiness")
            .status,
        STATUS_PENDING,
        "readiness bypassed the fresh DKG activation boundary"
    );
    assert_eq!(
        world.rpc.has_threshold_shares(restarted_port),
        Some(false),
        "validator exposed current threshold material after readiness but before fresh DKG"
    );
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

    assert!(
        world.localnet.validator_running(MISSED_VALIDATOR),
        "validator process exited during fresh DKG activation"
    );
    let canonical_checkpoint = world
        .rpc
        .finalized(primary)
        .expect("post-activation canonical checkpoint height");
    let canonical_checkpoint_hash = world
        .rpc
        .block_hash(primary, canonical_checkpoint)
        .expect("post-activation canonical checkpoint hash");
    wait_for_live_validator_checkpoint(
        world,
        MISSED_VALIDATOR,
        canonical_checkpoint,
        &canonical_checkpoint_hash,
        VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("rejoined validator lost canonical parity: {error}"));
    assert_eq!(
        world.rpc.has_threshold_shares(restarted_port),
        Some(true),
        "canonically caught-up ACTIVE validator has no current private threshold share"
    );
    let expected_material_version = world
        .rpc
        .consensus_status_field(primary, "vrfMaterialVersion")
        .expect("primary active VRF material version");
    let restarted_material_version = world
        .rpc
        .consensus_status_field(restarted_port, "vrfMaterialVersion")
        .expect("restarted validator active VRF material version");
    assert_eq!(
        restarted_material_version, expected_material_version,
        "restarted validator loaded a different active VRF material version"
    );
    assert_eq!(
        world.rpc.voter_miss_count(primary, &address),
        Some(voter_misses_before_readiness),
        "restarted validator accumulated voter misses through activation"
    );

    let full_node = state().full_node_index.expect("FullNode index");
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

    let completion_checkpoint = world
        .rpc
        .finalized(primary)
        .expect("scenario completion checkpoint height");
    let completion_checkpoint_hash = world
        .rpc
        .block_hash(primary, completion_checkpoint)
        .expect("scenario completion checkpoint hash");
    wait_for_live_validator_checkpoint(
        world,
        MISSED_VALIDATOR,
        completion_checkpoint,
        &completion_checkpoint_hash,
        VALIDATOR_RECOVERY_CATCH_UP_TIMEOUT,
    )
    .unwrap_or_else(|error| panic!("validator did not remain live through completion: {error}"));
    assert_eq!(
        world.rpc.has_threshold_shares(restarted_port),
        Some(true),
        "validator lost its current private threshold share before scenario completion"
    );
    let expected_completion_material_version = world
        .rpc
        .consensus_status_field(primary, "vrfMaterialVersion")
        .expect("primary completion VRF material version");
    let restarted_completion_material_version = world
        .rpc
        .consensus_status_field(restarted_port, "vrfMaterialVersion")
        .expect("validator completion VRF material version");
    assert_eq!(
        restarted_completion_material_version, expected_completion_material_version,
        "validator VRF material diverged before scenario completion"
    );
    assert_eq!(
        world.rpc.voter_miss_count(primary, &address),
        Some(voter_misses_before_readiness),
        "restarted validator accumulated voter misses after activation"
    );
}

#[cfg(test)]
mod tests {
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
