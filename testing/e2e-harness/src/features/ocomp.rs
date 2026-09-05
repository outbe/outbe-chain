//! OCM-24 process-topology acceptance steps.
//!
//! The scenario extends the normal localnet lifecycle and launches only the
//! production `outbe-ocomp` executable. It cannot construct jobs, results,
//! roots or chain state.

use std::{
    path::{Path, PathBuf},
    str::FromStr as _,
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use cucumber::{given, then, when};
use eyre::{ensure, eyre};
use outbe_chain_constants::GenesisProtocolParametersV1;
use outbe_node::ocomp::retention::{inspect_retention_journal, PinReleaseReason, PinStateV1};
use outbe_ocomp_protocol::{
    profile::poc_schema_limits,
    result::{ActiveNodSetV1, LysisResultV1, NodActionV1, NodMembershipProofV1, ResultChunkV1},
    state::{OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    system_carrier::{MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS, OCOMP_SYSTEM_CARRIER_GAS_LIMIT},
    vote::ResultVoteV1,
};
use outbe_ocompregistry::{OcompProtocolAuthorityV1, OcompRequestProfile, OcompSuccessorV1};
use outbe_primitives::time::WorldwideDay;

use crate::features::common::{bootstrap_localnet, start_bootstrapped_localnet};
use crate::internal::addresses::UPDATE_ADDR;
use crate::internal::eth;
use crate::world::localnet::StartOpts;
use crate::world::ocomp::{
    OcompMeasurementForkV1, OcompNodeFacingResumePlan, OcompProcessFault, OcompProcessRole,
};
use crate::world::ocomp::{
    OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS, OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS,
    OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS, OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS,
    OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO, OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE,
    OCOMP_TEST_EPOCH_LENGTH_BLOCKS,
};
use crate::world::state::{
    MetadosisFinalizedPointV1, MetadosisFreshLifecycleObservationV1, MetadosisTimeControlEpochV1,
    OcompExecutionTraceObservationV1,
};
use crate::world::World;

const OCOMP_CAPACITY_TRIBUTE_COUNT: usize = 257;
const OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS: u64 = 300;
const OCOMP_CAPACITY_NOD_MATERIALIZATION_TIMEOUT_SECS: u64 = 600;
// The SGX capacity lane intentionally submits 257 encrypted transactions on a
// loaded four-validator host. Keep its per-receipt bound at ten minutes while
// leaving ordinary scenario receipt waits unchanged (500 ms per attempt).
const OCOMP_CAPACITY_RECEIPT_ATTEMPTS: u32 = 1_200;
// The capacity scenario proves the protocol path and the 256+1 shard boundary,
// not Tribute burst throughput. Keep at most two offers in flight until
// outbe-chain-08n.6 gives blocking TEE work a production-safe block budget.
const OCOMP_CAPACITY_SUBMISSION_CONCURRENCY: usize = 2;
// The capacity lane proves the second 256-Tribute work shard using 129
// gas-bounded submission rounds. Keep the logical genesis window short, while
// leaving enough room for debug-build block production before controlled time
// advances the same chain to the next phase.
// Sequential real-SGX offers remain inside the genesis-bound phase window;
// the controlled-time step advances immediately after all receipts arrive.
const METADOSIS_CAPACITY_OFFERING_SECONDS: u64 = 3_600;
// Exact WorldwideDay VWAP formation always spans the canonical 50-hour window.
// The scenario advances that interval with the controlled logical-time ratchet;
// it must never shorten the consensus constant merely to make the E2E faster.
const METADOSIS_FRESH_FORMING_SECONDS: u64 =
    outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS;
const OCOMP_TRACE_FOLLOWER_SLOT: usize = 14;
// A one-Tribute scenario can reach request publication well before its
// genesis-bound offering window closes, so the bounded wait includes the
// remaining phase interval plus finalization/request publication slack.
const OCOMP_JOB_REQUEST_TIMEOUT_SECS: u64 = 300;
const OCOMP_PROGRESS_STALL_TIMEOUT_SECS: u64 = 120;
// A WWD begins at 10:00 UTC on the previous civil date (UTC+14 midnight), while
// the block-1 bootstrap derives its first key from the raw UTC civil date.
// Starting 15 hours into the WWD places block 1 at 01:00 UTC on that same key:
// both date conventions select the fixture WWD and it remains inside FORMING.
const METADOSIS_INITIAL_WWD_ELAPSED_SECS: u64 = 15 * 3_600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedCompletionDecision {
    Complete,
    Continue,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgressWaitDecision {
    Reached,
    Progressed,
    Waiting,
    Stalled,
}

fn monotonic_progress_decision(
    current: u64,
    target: u64,
    previous: u64,
    now: Instant,
    progress_deadline: Instant,
) -> ProgressWaitDecision {
    if current >= target {
        ProgressWaitDecision::Reached
    } else if current > previous {
        ProgressWaitDecision::Progressed
    } else if now >= progress_deadline {
        ProgressWaitDecision::Stalled
    } else {
        ProgressWaitDecision::Waiting
    }
}

fn wait_for_common_finalized_checkpoint(
    world: &mut World,
    target: u64,
    fault_label: &str,
) -> crate::world::rpc::FinalizedCheckpoint {
    let ports = world.validators.committee_ports();
    let mut previous_common_height = 0_u64;
    let mut progress_deadline =
        Instant::now() + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
    loop {
        let observations = ports
            .iter()
            .map(|&port| (port, world.rpc.finalized_result(port)))
            .collect::<Vec<_>>();
        let common_height = observations
            .iter()
            .map(|(_, height)| height.as_ref().ok().copied())
            .collect::<Option<Vec<_>>>()
            .and_then(|heights| heights.into_iter().min());
        let now = Instant::now();
        if let Some(current) = common_height {
            match monotonic_progress_decision(
                current,
                target,
                previous_common_height,
                now,
                progress_deadline,
            ) {
                ProgressWaitDecision::Reached => {
                    world
                        .rpc
                        .wait_finalized_checkpoint(&ports, target, 1)
                        .unwrap_or_else(|error| {
                            panic!(
                                "{fault_label} nodes reached h{target} but disagree on the common finalized checkpoint: {error:#}"
                            )
                        });
                    let expected =
                        world
                            .rpc
                            .checkpoint_at(ports[0], target)
                            .unwrap_or_else(|error| {
                                panic!("read {fault_label} checkpoint h{target}: {error:#}")
                            });
                    for &port in &ports[1..] {
                        let observed = world.rpc.checkpoint_at(port, target).unwrap_or_else(
                            |error| {
                                panic!(
                                    "read {fault_label} checkpoint h{target} on port {port}: {error:#}"
                                )
                            },
                        );
                        assert_eq!(
                            observed, expected,
                            "{fault_label} nodes disagree at exact finalized h{target}"
                        );
                    }
                    return expected;
                }
                ProgressWaitDecision::Progressed => {
                    previous_common_height = current;
                    progress_deadline =
                        now + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
                }
                ProgressWaitDecision::Waiting => {}
                ProgressWaitDecision::Stalled => {
                    panic!("{fault_label} finality stalled below h{target}: {observations:?}");
                }
            }
        } else if now >= progress_deadline {
            panic!(
                "{fault_label} finality became unobservable for {}s below h{target}: {observations:?}",
                OCOMP_PROGRESS_STALL_TIMEOUT_SECS
            );
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .unwrap_or_else(|error| panic!("{fault_label} OCOMP role exited: {error:#}"));
        sleep(Duration::from_millis(250));
    }
}

fn bounded_completion_decision(
    all_complete: bool,
    now: Instant,
    deadline: Instant,
) -> BoundedCompletionDecision {
    if all_complete {
        BoundedCompletionDecision::Complete
    } else if now >= deadline {
        BoundedCompletionDecision::TimedOut
    } else {
        BoundedCompletionDecision::Continue
    }
}

#[given("a fresh four-validator OCOMP measurement localnet")]
fn fresh_ocomp_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, None, None, false);
}

#[given("a fresh four-validator OCOMP public measurement localnet")]
fn fresh_ocomp_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), None, false);
}

#[given("a fresh four-validator OCOMP short-window public measurement localnet")]
fn fresh_ocomp_short_window_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), Some(6), false);
}

#[given("a fresh four-validator OCOMP short-window public recovery localnet")]
fn fresh_ocomp_short_window_public_recovery_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), Some(6), true);
}

#[given("a fresh four-validator OCOMP public capacity localnet")]
fn fresh_ocomp_public_capacity_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(OCOMP_CAPACITY_TRIBUTE_COUNT), None, false);
}

#[given("a fresh four-validator OCOMP dynamic-membership localnet with two scheduled jobs")]
fn fresh_ocomp_dynamic_membership_localnet(world: &mut World) {
    bootstrap_localnet(
        world,
        6,
        &[
            (
                "TESTNET_EPOCH_LENGTH_BLOCKS",
                OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
            ),
            (
                "TESTNET_DKG_PREPARE_WINDOW_BLOCKS",
                OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS.to_string(),
            ),
            (
                "TESTNET_OCOMP_VOTE_WINDOW_BLOCKS",
                OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS.to_string(),
            ),
            ("TESTNET_DEV_FELONY_THRESHOLD", "10".to_owned()),
        ],
    );
    let now_secs = unix_time_secs();
    let mut start_opts = StartOpts::near_next_utc_day_with_lead(6, now_secs, 180);
    let offset = start_opts
        .unix_time_offset_secs
        .expect("dynamic membership clock offset");
    world
        .localnet
        .shift_genesis_timestamp(offset)
        .expect("shift dynamic OCOMP genesis before deriving fork identity");
    start_opts.genesis_timestamp_pre_shifted = true;

    let prepared = world
        .ocomp
        .prepare_dynamic_membership_fork_install()
        .expect("prepare two public jobs around the real membership boundary");
    world.state.ocomp_dynamic_worldwide_days = vec![
        prepared.first_worldwide_day.value(),
        prepared.second_worldwide_day.value(),
    ];
    world.state.ocomp_dynamic_processing_times = vec![
        prepared.first_processing_time,
        prepared.second_processing_time,
    ];
    world
        .localnet
        .bind_tee_genesis()
        .expect("bind canonical TEE genesis after dynamic OCOMP manifest");
    launch_prepared_ocomp(world, &mut start_opts, &prepared.fork, true);
    wait_for_finalized_ocomp_activation(world);
}

#[given("a fresh four-validator Metadosis capacity localnet at FORMING")]
fn fresh_metadosis_capacity_localnet_at_forming(world: &mut World) {
    bootstrap_localnet(
        world,
        6,
        &[
            (
                "TESTNET_EPOCH_LENGTH_BLOCKS",
                OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
            ),
            (
                "TESTNET_METADOSIS_FORMING_SECONDS",
                METADOSIS_FRESH_FORMING_SECONDS.to_string(),
            ),
            (
                "TESTNET_METADOSIS_OFFERING_SECONDS",
                METADOSIS_CAPACITY_OFFERING_SECONDS.to_string(),
            ),
        ],
    );
    let wwd = world
        .state
        .wwd
        .as_deref()
        .expect("fresh Metadosis WorldwideDay")
        .parse::<WorldwideDay>()
        .expect("valid fresh Metadosis WorldwideDay");
    let now_secs = unix_time_secs();
    let initial_timestamp = wwd
        .start_timestamp()
        .checked_add(METADOSIS_INITIAL_WWD_ELAPSED_SECS)
        .expect("fresh Metadosis initial logical time");
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp),
        wwd,
        "UTC+14 timestamp mapping must select the fixture WWD"
    );
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp.saturating_sub(14 * 3_600)),
        wwd,
        "block-1 UTC date mapping must select the fixture WWD"
    );
    let initial_offset = logical_time_offset(initial_timestamp, now_secs);
    world
        .localnet
        .shift_genesis_timestamp(initial_offset)
        .expect("shift fresh Metadosis genesis before deriving fork identity");
    world.state.metadosis_fresh_initial_timestamp = Some(initial_timestamp);
    world.state.metadosis_fresh_initial_unix_time_offset_secs = Some(initial_offset);

    let (prepared, private_keys) = world
        .ocomp
        .prepare_fresh_metadosis_capacity_fork_install(OCOMP_CAPACITY_TRIBUTE_COUNT)
        .expect("prepare runtime-created fresh Metadosis capacity fork");
    world.state.ocomp_capacity_tribute_private_keys = private_keys;
    let mut start_opts = StartOpts {
        voting_window: Some(6),
        unix_time_offset_secs: Some(initial_offset),
        genesis_timestamp_pre_shifted: true,
        is_txpool_eviction_profile: false,
    };
    launch_prepared_ocomp(world, &mut start_opts, &prepared, true);
    wait_for_finalized_ocomp_activation(world);
}

fn start_ocomp_measurement_localnet(
    world: &mut World,
    public_capacity_tribute_count: Option<usize>,
    vote_window_blocks: Option<u64>,
    seed_recovery_day: bool,
) {
    assert!(
        !seed_recovery_day || public_capacity_tribute_count == Some(0),
        "the second recovery WWD belongs only to the one-Tribute public fixture"
    );
    let shorten_public_day = public_capacity_tribute_count.is_some();
    let mut tuning = vec![(
        "TESTNET_EPOCH_LENGTH_BLOCKS",
        OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
    )];
    if let Some(window) = vote_window_blocks {
        tuning.push(("TESTNET_OCOMP_VOTE_WINDOW_BLOCKS", window.to_string()));
    }
    bootstrap_localnet(world, 6, &tuning);
    let mut start_opts = if shorten_public_day {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_secs();
        let boundary_lead_secs = if public_capacity_tribute_count == Some(0) {
            OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS
        } else {
            OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
        };
        let mut opts = StartOpts::near_next_utc_day_with_lead(6, now_secs, boundary_lead_secs);
        let offset = opts
            .unix_time_offset_secs
            .expect("public measurement clock offset");
        world
            .localnet
            .shift_genesis_timestamp(offset)
            .expect("shift public measurement genesis before deriving fork identity");
        opts.genesis_timestamp_pre_shifted = true;
        opts
    } else {
        StartOpts::default()
    };
    let measurement_fork = match public_capacity_tribute_count {
        Some(0) if seed_recovery_day => world
            .ocomp
            .prepare_public_recovery_fork_install()
            .expect("publish the immutable two-day public recovery fork before node launch"),
        Some(0) => world
            .ocomp
            .prepare_public_measurement_fork_install()
            .expect("publish the immutable public measurement fork before node launch"),
        Some(tribute_count) => {
            let (prepared, private_keys) = world
                .ocomp
                .prepare_public_capacity_fork_install(tribute_count)
                .expect("fund capacity owners and publish the immutable measurement fork");
            world.state.ocomp_capacity_tribute_private_keys = private_keys;
            prepared
        }
        None => world
            .ocomp
            .prepare_measurement_fork_install()
            .expect("publish the immutable measurement fork before node launch"),
    };
    if let Some(worldwide_day) = measurement_fork.public_worldwide_day {
        world.state.wwd = Some(worldwide_day.to_string());
    }
    world
        .localnet
        .bind_tee_genesis()
        .expect("bind canonical TEE genesis after installing the mandatory OCOMP manifest");
    launch_prepared_ocomp(world, &mut start_opts, &measurement_fork, true);
    if shorten_public_day {
        wait_for_finalized_ocomp_activation(world);
    }
}

fn launch_prepared_ocomp(
    world: &mut World,
    start_opts: &mut StartOpts,
    prepared: &OcompMeasurementForkV1,
    activate_workers: bool,
) {
    let expected_identity = prepared.launch_identity();
    assert!(
        world.state.ocomp_activation_height.is_none(),
        "scenario already selected an immutable OCOMP activation height"
    );
    world.state.ocomp_activation_height = Some(prepared.install.activation_height);
    start_bootstrapped_localnet(world, start_opts);

    let primary = world.validators.primary_port();
    let chain_id = world
        .rpc
        .chain_id(primary)
        .expect("read measurement chain id from public RPC");
    let genesis_hash = world
        .rpc
        .block_hash(primary, 0)
        .and_then(|hash| B256::from_str(&hash).ok())
        .expect("read measurement genesis hash from public RPC");
    assert_eq!(chain_id, expected_identity.chain_id);
    assert_eq!(genesis_hash, expected_identity.genesis_hash);
    let identity = expected_identity;
    world
        .ocomp
        .install_ocomp_delegate_bindings()
        .expect("install distinct role-scoped OCOMP transaction signers");
    world
        .ocomp
        .start_validator_roles(identity)
        .expect("start all production node-facing OCOMP roles");
    if activate_workers {
        for validator_index in 0..world.validators.size() {
            let validator_index = u8::try_from(validator_index)
                .expect("configured validator index fits the OCOMP harness wire format");
            world
                .ocomp
                .activate_worker(validator_index, 0, identity)
                .unwrap_or_else(|error| {
                    panic!("activate validator-{validator_index} production worker: {error}")
                });
        }
    }
}

#[then("every OCOMP transaction signer is distinct and scoped only to the OCOMP role")]
fn every_ocomp_transaction_signer_is_role_scoped(world: &mut World) {
    world
        .ocomp
        .verify_ocomp_delegate_bindings()
        .expect("verify distinct role-scoped OCOMP transaction signers");
}

fn wait_for_finalized_ocomp_activation(world: &mut World) {
    let activation_height = world
        .state
        .ocomp_activation_height
        .expect("prepared OCOMP activation height");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let finalized = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| world.rpc.finalized(port))
            .collect::<Vec<_>>();
        if finalized
            .iter()
            .all(|height| height.is_some_and(|height| height >= activation_height))
        {
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive until the immutable fork is active");
        assert!(
            Instant::now() < deadline,
            "OCOMP fork did not finalize on every validator before public Tribute submission: \
             expected height {activation_height}, observed {finalized:?}"
        );
        sleep(Duration::from_millis(250));
    }
}

#[when("a fifth node syncs as a non-voting FullNode")]
fn fifth_node_syncs_as_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner index fits OCOMP harness wire");
    let primary = world.validators.primary_port();
    let target = world
        .rpc
        .head(primary)
        .expect("primary head before FullNode sync");
    let ocomp_args = world
        .ocomp
        .stage_keyless_full_node_domain(validator_index)
        .expect("stage keyless FullNode OCOMP compute profile outside ACTIVE topology");
    world
        .localnet
        .launch_joiner_full_node(index, 0, &ocomp_args)
        .expect("launch the fifth slot without validator credentials");
    world
        .ocomp
        .start_keyless_full_node_roles(validator_index)
        .expect("start the FullNode external SnapshotExporter and Worker");
    let joined = world
        .rpc
        .wait_block(world.validators.http_port(index), target, 60)
        .expect("FullNode syncs canonical blocks");
    assert!(joined >= target);
}

#[then("the fifth node has canonical state parity without OCOMP vote capability")]
fn fifth_full_node_has_state_but_no_vote_capability(world: &mut World) {
    let index = world.validators.joiner_index();
    let primary = world.validators.primary_port();
    let follower = world.validators.http_port(index);
    let height = world
        .rpc
        .finalized(follower)
        .expect("FullNode finalized height");
    assert_eq!(
        world.rpc.state_root(follower, height),
        world.rpc.state_root(primary, height)
    );
    assert_eq!(world.rpc.active_count(primary), Some(4));
    let data_dir = world.validators.data_dir(index);
    let validator_dir = data_dir.parent().expect("validator data directory parent");
    assert!(!validator_dir.join("ocomp-key-v1.hex").exists());
    assert!(validator_dir.join("signing-key.hex").is_file());
    assert!(validator_dir.join("evm-key.hex").is_file());
    assert!(world.ocomp.process_records().iter().any(|record| {
        record.validator_index == Some(u8::try_from(index).expect("joiner index fits u8"))
            && record.role == OcompProcessRole::Worker
            && record.worker_ordinal == Some(0)
            && record.stopped_at_millis.is_none()
    }));
}

#[when("the synced node completes OCOMP-ready validator admission")]
fn synced_node_completes_ocomp_validator_admission(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner index fits OCOMP harness wire");
    let primary = world.validators.primary_port();

    // Generate the validator/OCOMP material and complete REGISTERED admission
    // while the same durable datadir is still advancing in certified FullNode
    // follower mode. REGISTERED is deliberately outside the DKG target, so this
    // preparation cannot change membership. Keeping the follower alive avoids
    // losing an epoch while keygen, enclave startup and `tee join` complete.
    world
        .localnet
        .provision_joiner_registration(index)
        .expect("register BLS and OCOMP identity while FullNode keeps syncing");
    world
        .ocomp
        .stage_joiner_domain_material(validator_index)
        .expect("stage the registered OCOMP key without changing membership");

    let epoch_length = world
        .localnet
        .epoch_length_blocks()
        .expect("canonical ValidatorSet epoch length");
    let safe_window_deadline =
        Instant::now() + Duration::from_secs(epoch_length.saturating_mul(3).max(30));
    loop {
        let primary_finalized = world
            .rpc
            .finalized(primary)
            .expect("canonical finality before validator-mode restart");
        let follower = world.validators.http_port(index);
        let follower_finalized = world
            .rpc
            .finalized(follower)
            .expect("FullNode finality before validator-mode restart");
        let follower_state_matches = follower_finalized <= primary_finalized
            && world.rpc.state_root(follower, follower_finalized)
                == world.rpc.state_root(primary, follower_finalized);
        if joiner_restart_is_in_safe_early_epoch_window(
            primary_finalized,
            follower_finalized,
            epoch_length,
        ) && follower_state_matches
        {
            break;
        }
        assert!(
            Instant::now() < safe_window_deadline,
            "FullNode did not reach a safe early-epoch validator admission window"
        );
        sleep(Duration::from_millis(250));
    }

    // Only the process-role handover happens in the post-boundary window. The
    // validator therefore recovers the same epoch the committee is running and
    // has the full prepare window to finalize stake/readiness before freeze.
    world
        .ocomp
        .stop_keyless_full_node_roles(validator_index)
        .expect("stop keyless FullNode roles before validator-mode restart");
    world.localnet.stop_joiner_full_node(index);
    world
        .localnet
        .launch_joiner(index, &[])
        .expect("restart the synced datadir in validator mode");
    let key = world.validators.joiner().evm_key().expect("joiner EVM key");
    let address = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(address.clone());
    world.rpc.stake(&key, 1_000).expect("stake joiner");
    assert_eq!(world.rpc.validator_status(primary, &address), Some(1));
    assert!(!world
        .rpc
        .is_participant(primary, &address)
        .expect("observe consensus participation"));
    world
        .rpc
        .confirm_ready(&key)
        .expect("confirm OCOMP registration and readiness");
    assert!(
        world
            .rpc
            .wait_participant(primary, &address, 70)
            .expect("wait for observable consensus participation"),
        "certified DKG boundary did not activate the joiner"
    );
    assert_eq!(world.rpc.validator_status(primary, &address), Some(2));
    world
        .ocomp
        .add_active_validator_domain(validator_index)
        .expect("append only the now-ACTIVE validator domain");
    world
        .ocomp
        .install_ocomp_delegate_bindings()
        .expect("install the fifth operational OCOMP delegate");
    world
        .ocomp
        .start_active_validator_roles(validator_index)
        .expect("start the fifth validator OCOMP roles");
    let identity = world
        .ocomp
        .launch_identity()
        .expect("OCOMP launch identity remains pinned");
    world
        .ocomp
        .activate_worker(validator_index, 0, identity)
        .expect("activate fifth validator worker");
}

fn joiner_restart_is_in_safe_early_epoch_window(
    primary_finalized_height: u64,
    follower_finalized_height: u64,
    epoch_length: u64,
) -> bool {
    assert!(epoch_length > 0, "epoch length is a consensus precondition");
    let last_safe_remainder = epoch_length / 2;
    primary_finalized_height / epoch_length == follower_finalized_height / epoch_length
        && (1..=last_safe_remainder).contains(&(primary_finalized_height % epoch_length))
        && (1..=last_safe_remainder).contains(&(follower_finalized_height % epoch_length))
}

#[then("the certified boundary adds exactly one fifth OCOMP validator domain")]
fn certified_boundary_adds_fifth_ocomp_domain(world: &mut World) {
    let primary = world.validators.primary_port();
    assert_eq!(world.rpc.active_count(primary), Some(5));
    let evidence = world.ocomp.evidence_snapshot().expect("dynamic topology");
    assert_eq!(evidence.domain_roots.len(), 5);
    for role in [OcompProcessRole::SnapshotExporter, OcompProcessRole::Worker] {
        assert!(world.ocomp.process_records().iter().any(|record| {
            record.validator_index == Some(4)
                && record.role == role
                && record.stopped_at_millis.is_none()
        }));
    }
}

#[then("job B opens with five members and quorum four while job A remains four of three")]
fn job_b_uses_the_new_snapshot_while_job_a_keeps_the_old_one(world: &mut World) {
    let primary = world.validators.primary_port();
    let certified_five_member_height = world
        .rpc
        .finalized(primary)
        .expect("finalized five-member activation boundary");
    assert_eq!(
        world.rpc.active_count(primary),
        Some(5),
        "Job B may only be released after the five-member set is ACTIVE"
    );
    advance_dynamic_membership_to_next_daily_cycle(world);

    let job_a_request = world
        .state
        .ocomp_dynamic_job_requests
        .first()
        .cloned()
        .expect("finalized job A request");
    let job_b_wwd = *world
        .state
        .ocomp_dynamic_worldwide_days
        .get(1)
        .expect("job B WorldwideDay");
    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(world.validators.joiner_index()));
    let deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let mut last_observation = "no finalized job B request observed".to_owned();

    let (
        job_b_request,
        job_a_record,
        job_b_record,
        job_a_votes,
        job_b_votes,
        joiner_participant_index,
    ) = loop {
        let requests = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_for_worldwide_day_on(
                        port,
                        job_a_request.request_height + 1,
                        job_b_wwd,
                    )
                    .unwrap_or_else(|error| panic!("observe job B on port {port}: {error:#}"))
            })
            .collect::<Vec<_>>();
        if requests.iter().all(Option::is_some) {
            let request = requests[0].clone().expect("all job B requests are present");
            assert!(
                requests
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&request)),
                "validators expose different finalized job B requests"
            );
            if request.worldwide_day == job_b_wwd {
                assert!(
                    request.request_height > certified_five_member_height,
                    "Job B request at height {} predates the certified five-member barrier at height {}; the harness released it too early",
                    request.request_height,
                    certified_five_member_height
                );
                let job_a_records = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_job_record_on(port, job_a_request.intent_id)
                    })
                    .collect::<Vec<_>>();
                let job_b_records = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_job_record_on(port, request.intent_id)
                    })
                    .collect::<Vec<_>>();
                let job_a_statuses = job_a_records
                    .iter()
                    .map(|record| record.as_ref().map(|record| record.status))
                    .collect::<Vec<_>>();
                let job_b_statuses = job_b_records
                    .iter()
                    .map(|record| record.as_ref().map(|record| record.status))
                    .collect::<Vec<_>>();
                last_observation = format!(
                    "job_b_request=({:#x}, {}), job_a_statuses={job_a_statuses:?}, \
                     job_b_statuses={job_b_statuses:?}",
                    request.intent_id, request.worldwide_day
                );
                if job_a_records.iter().all(|record| {
                    record
                        .as_ref()
                        .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
                }) && job_b_records.iter().all(|record| {
                    record
                        .as_ref()
                        .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
                }) {
                    let job_a_record = job_a_records[0]
                        .clone()
                        .expect("all job A records are present");
                    let job_b_record = job_b_records[0]
                        .clone()
                        .expect("all job B records are present");
                    assert!(
                        job_a_records
                            .iter()
                            .all(|observed| observed.as_ref() == Some(&job_a_record)),
                        "validators expose different canonical job A records after activation"
                    );
                    assert!(
                        job_b_records
                            .iter()
                            .all(|observed| observed.as_ref() == Some(&job_b_record)),
                        "validators expose different canonical job B records"
                    );
                    let job_a_id = job_a_record
                        .finalized
                        .as_ref()
                        .expect("job A finalized intent")
                        .job_id;
                    let job_b_id = job_b_record
                        .finalized
                        .as_ref()
                        .expect("job B finalized intent")
                        .job_id;
                    let job_a_accountability = ports
                        .iter()
                        .copied()
                        .map(|port| {
                            world
                                .rpc
                                .finalized_ocomp_vote_accountability_on(port, job_a_id)
                        })
                        .collect::<Vec<_>>();
                    let job_b_accountability = ports
                        .iter()
                        .copied()
                        .map(|port| {
                            world
                                .rpc
                                .finalized_ocomp_vote_accountability_on(port, job_b_id)
                        })
                        .collect::<Vec<_>>();
                    let job_a_slots = job_a_accountability
                        .iter()
                        .map(|accountability| {
                            accountability
                                .as_ref()
                                .map(|accountability| accountability.slot_validator_indexes.clone())
                        })
                        .collect::<Vec<_>>();
                    let job_b_slots = job_b_accountability
                        .iter()
                        .map(|accountability| {
                            accountability
                                .as_ref()
                                .map(|accountability| accountability.slot_validator_indexes.clone())
                        })
                        .collect::<Vec<_>>();
                    last_observation = format!(
                        "job_b_request=({:#x}, {}), job_a_statuses={job_a_statuses:?}, \
                         job_b_statuses={job_b_statuses:?}, job_a_slots={job_a_slots:?}, \
                         job_b_slots={job_b_slots:?}",
                        request.intent_id, request.worldwide_day
                    );
                    if job_a_accountability.iter().all(Option::is_some)
                        && job_b_accountability.iter().all(Option::is_some)
                    {
                        let job_a_votes = job_a_accountability[0]
                            .clone()
                            .expect("all job A accountability records are present");
                        let job_b_votes = job_b_accountability[0]
                            .clone()
                            .expect("all job B accountability records are present");
                        assert!(
                            job_a_accountability
                                .iter()
                                .all(|observed| observed.as_ref() == Some(&job_a_votes)),
                            "validators expose different job A accountability"
                        );
                        assert!(
                            job_b_accountability
                                .iter()
                                .all(|observed| observed.as_ref() == Some(&job_b_votes)),
                            "validators expose different job B accountability"
                        );
                        let finalized_height = world
                            .rpc
                            .finalized_result(primary)
                            .expect("observe finalized height for job B accountability");
                        if let Some(joiner_vote) = finalized_vote_for_delegate_on_job(
                            world,
                            request.request_height,
                            finalized_height,
                            world.validators.joiner_index(),
                            job_b_id,
                        ) {
                            if let Some(joiner_participant_index) =
                                accountability_slot_for_vote(&job_b_votes, &joiner_vote)
                            {
                                if dynamic_pre_restart_vote_baseline_ready(
                                    job_a_votes.slot_validator_indexes.len(),
                                    job_b_votes.slot_validator_indexes.len(),
                                    true,
                                ) {
                                    break (
                                        request,
                                        job_a_record,
                                        job_b_record,
                                        job_a_votes,
                                        job_b_votes,
                                        joiner_participant_index,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "job B did not open and accept the fifth validator's vote before timeout: \
             {last_observation}"
        );
        sleep(Duration::from_millis(500));
    };

    assert_eq!(job_a_record.intent.result_member_count, 4);
    assert_eq!(job_a_record.intent.result_quorum_threshold, 3);
    assert_eq!(job_a_votes.member_count, 4);
    assert_eq!(job_a_votes.quorum_threshold, 3);
    let job_a_id = job_a_record
        .finalized
        .as_ref()
        .expect("job A finalized intent")
        .job_id;
    assert!(
        !dynamic_vote_submission_path(world, world.validators.joiner_index(), job_a_id).exists(),
        "the fifth validator must not create a vote submission for job A's historical snapshot"
    );

    assert_eq!(job_b_record.intent.wwd, job_b_wwd);
    assert_eq!(job_b_record.intent.result_member_count, 5);
    assert_eq!(job_b_record.intent.result_quorum_threshold, 4);
    assert_eq!(job_b_votes.member_count, 5);
    assert_eq!(job_b_votes.quorum_threshold, 4);
    assert!(
        job_b_votes
            .slot_validator_indexes
            .contains(&joiner_participant_index),
        "the fifth validator's signed public vote must be accepted at its canonical snapshot \
         index {joiner_participant_index}"
    );
    assert!(
        job_b_record.intent.result_validator_set_epoch
            > job_a_record.intent.result_validator_set_epoch
    );
    assert_ne!(
        job_b_record.intent.result_committee_set_hash,
        job_a_record.intent.result_committee_set_hash
    );
    assert_ne!(
        job_b_record.intent.result_ocomp_binding_hash,
        job_a_record.intent.result_ocomp_binding_hash
    );
    world.state.ocomp_dynamic_job_requests.push(job_b_request);
}

fn advance_dynamic_membership_to_next_daily_cycle(world: &mut World) {
    const SECONDS_PER_DAY: u64 = 86_400;

    let primary = world.validators.primary_port();
    let before_restart = world
        .rpc
        .finalized(primary)
        .expect("canonical finality before the second dynamic OCOMP day");
    let current_timestamp = world
        .rpc
        .block_timestamp(primary, before_restart)
        .expect("canonical timestamp before the second dynamic OCOMP day");
    let next_daily_cycle = current_timestamp
        .checked_div(SECONDS_PER_DAY)
        .and_then(|day| day.checked_add(1))
        .and_then(|day| day.checked_mul(SECONDS_PER_DAY))
        .and_then(|midnight| midnight.checked_add(1))
        .expect("next UTC daily Cycle timestamp");
    let refresh_timestamp = dynamic_oracle_refresh_timestamp(next_daily_cycle);
    assert!(
        refresh_timestamp > current_timestamp,
        "dynamic Oracle refresh must remain ahead of canonical time: current={current_timestamp} refresh={refresh_timestamp}"
    );

    // Job B belongs to the following WorldwideDay. Metadosis deliberately
    // settles READY days only from its daily Cycle handler. First stage a real
    // feeder publication inside the final half of the six-hour FX TTL, then
    // cross the production daily boundary. The custom restart preserves the
    // intentionally stopped validator-2/3 OCOMP Workers.
    restart_dynamic_membership_nodes_at(
        world,
        refresh_timestamp,
        before_restart,
        "pre-boundary Oracle refresh",
    );
    let before_daily_cycle = world
        .rpc
        .finalized(primary)
        .expect("canonical finality after the staged Oracle refresh");
    restart_dynamic_membership_nodes_at(
        world,
        next_daily_cycle,
        before_daily_cycle,
        "next daily Cycle",
    );
}

fn restart_dynamic_membership_nodes_at(
    world: &mut World,
    requested_timestamp: u64,
    before_restart: u64,
    phase: &str,
) {
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    assert!(
        price_publication.is_some(),
        "dynamic OCOMP {phase} requires the production price feeder"
    );
    let ocomp_roles = world
        .ocomp
        .suspend_node_facing_roles()
        .unwrap_or_else(|error| panic!("suspend exact OCOMP live roles before {phase}: {error:#}"));
    let offset = logical_time_offset(requested_timestamp, unix_time_secs());
    let joiner_index = world.validators.joiner_index();
    world
        .localnet
        .stop_joiner(joiner_index)
        .unwrap_or_else(|error| panic!("stop the fifth validator before {phase}: {error:#}"));
    world
        .localnet
        .restart_committee_at_unix_time_offset(offset)
        .unwrap_or_else(|error| panic!("restart the original committee for {phase}: {error:#}"));
    world
        .localnet
        .launch_joiner(joiner_index, &[])
        .unwrap_or_else(|error| panic!("restart the fifth validator for {phase}: {error:#}"));

    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(joiner_index));
    for port in ports {
        assert!(
            world
                .rpc
                .wait_finalized_at_least(port, before_restart.saturating_add(1), 240),
            "validator on port {port} did not resume finality across {phase}"
        );
    }
    let pending =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication)
            .expect("dynamic OCOMP feeder was running before the clock restart");
    world
        .ocomp
        .resume_node_facing_roles(ocomp_roles)
        .unwrap_or_else(|error| panic!("resume exact OCOMP live roles after {phase}: {error:#}"));
    while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
        sleep(Duration::from_millis(250));
    }
}

fn dynamic_job_record(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
) -> OcompJobRecordV1 {
    world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .expect("dynamic OCOMP job record")
}

fn finalized_vote_for_delegate_on_job(
    world: &World,
    from_height: u64,
    to_height: u64,
    node_index: usize,
    job_id: B256,
) -> Option<ResultVoteV1> {
    let validator_index = u8::try_from(node_index).ok()?;
    let delegate = world.ocomp.ocomp_delegate_address(validator_index).ok()?;
    world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            world.validators.primary_port(),
            from_height,
            to_height,
        )?
        .into_iter()
        .filter(|transaction| transaction.success && transaction.signer == delegate)
        .find_map(|transaction| {
            let bytes = world.rpc.ocomp_result_vote_bytes_on(
                world.validators.primary_port(),
                transaction.transaction_hash,
            )?;
            let vote = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits()).ok()?;
            (vote.job_id == job_id).then_some(vote)
        })
}

fn accountability_slot_for_vote(
    accountability: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    vote: &ResultVoteV1,
) -> Option<u16> {
    let mut matches = accountability
        .slot_first_signatures
        .iter()
        .filter(|(_, signature)| signature.as_slice() == vote.signature_rs)
        .map(|(validator_index, _)| *validator_index);
    let participant_index = matches.next()?;
    matches.next().is_none().then_some(participant_index)
}

fn dynamic_pre_restart_vote_baseline_ready(
    job_a_vote_count: usize,
    job_b_vote_count: usize,
    joiner_vote_present: bool,
) -> bool {
    job_a_vote_count == 2 && job_b_vote_count == 3 && joiner_vote_present
}

fn dynamic_oracle_refresh_timestamp(next_daily_cycle: u64) -> u64 {
    const REFRESH_HEADROOM_SECS: u64 = 3 * 60 * 60;
    next_daily_cycle
        .checked_sub(REFRESH_HEADROOM_SECS)
        .expect("daily Cycle leaves three hours for an Oracle refresh")
}

fn singleton_participant_bitmap(member_count: u16, participant_index: u16) -> Vec<u8> {
    assert!(participant_index < member_count);
    let mut bitmap = vec![0_u8; usize::from(member_count).div_ceil(8)];
    bitmap[usize::from(participant_index / 8)] |= 1_u8 << (participant_index % 8);
    bitmap
}

fn dynamic_vote_submission_path(
    world: &World,
    node_index: usize,
    job_id: B256,
) -> std::path::PathBuf {
    let job_component = hex::encode(job_id.as_slice());
    world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("validator data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("supervisor-v1")
        .join("vote-submissions")
        .join(&job_component)
        .join(format!("{job_component}.vote.v1"))
}

fn local_result_path(world: &World, node_index: usize, job_id: B256) -> std::path::PathBuf {
    world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("node data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("node-v1")
        .join("local-results")
        .join(format!(
            "{}.lysis-result-v1.ocb1",
            hex::encode(job_id.as_slice())
        ))
}

fn full_node_local_result_path(world: &World, job_id: B256) -> std::path::PathBuf {
    local_result_path(world, world.validators.joiner_index(), job_id)
}

fn finalized_job_id(world: &World) -> B256 {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized OCOMP JobIntent");
    world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .and_then(|record| record.finalized.map(|finalized| finalized.job_id))
        .expect("finalized OCOMP job identity")
}

fn launch_preserved_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    world
        .localnet
        .launch_dcap_full_node(&format!("joiner-full-node-{index}"), index, 0)
        .expect("restart keyless FullNode with its preserved datadir and domain");
}

fn result_nod_actions_on(world: &World, node_index: usize, job_id: B256) -> Vec<NodActionV1> {
    let objects = world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("node data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("cas-v1")
        .join("objects");
    let limits = poc_schema_limits();
    let mut chunks = Vec::new();
    for prefix in std::fs::read_dir(&objects).expect("read OCOMP CAS prefix directory") {
        let prefix = prefix.expect("read OCOMP CAS prefix entry");
        assert!(
            prefix.file_type().expect("read CAS prefix type").is_dir(),
            "OCOMP CAS prefix is not a directory: {:?}",
            prefix.path()
        );
        for object in std::fs::read_dir(prefix.path()).expect("read OCOMP CAS object directory") {
            let object = object.expect("read OCOMP CAS object entry");
            assert!(
                object.file_type().expect("read CAS object type").is_file(),
                "OCOMP CAS object is not a file: {:?}",
                object.path()
            );
            let bytes = std::fs::read(object.path()).expect("read OCOMP CAS object");
            if let Ok(chunk) = ResultChunkV1::decode_canonical(&bytes, &limits) {
                if chunk.job_id == job_id {
                    chunks.push(chunk);
                }
            }
        }
    }
    chunks.sort_by_key(|chunk| chunk.chunk_ordinal);
    assert!(!chunks.is_empty(), "node has no result chunks for {job_id}");
    chunks
        .into_iter()
        .flat_map(|chunk| chunk.ordered_nod_actions)
        .collect()
}

#[then("the FullNode independently materializes job A without voting")]
fn full_node_materializes_job_a_without_voting(world: &mut World) {
    let request = world
        .state
        .ocomp_dynamic_job_requests
        .first()
        .expect("job A request before FullNode verification");
    let record = dynamic_job_record(world, request);
    let job_id = record
        .finalized
        .as_ref()
        .expect("job A finalized intent")
        .job_id;
    let path = full_node_local_result_path(world, job_id);
    let deadline = Instant::now() + Duration::from_secs(120);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "keyless FullNode did not publish canonical job A result at {}",
            path.display()
        );
        sleep(Duration::from_millis(250));
    }
    let encoded = std::fs::read(&path).expect("read FullNode canonical job A result");
    let result = LysisResultV1::decode_canonical(&encoded, &poc_schema_limits())
        .expect("decode FullNode canonical job A result");
    assert_eq!(result.job_id, job_id);
    assert_ne!(
        result
            .result_digest(&poc_schema_limits())
            .expect("validate FullNode job A result"),
        B256::ZERO
    );

    assert!(
        !dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists(),
        "keyless FullNode must compute job A without creating a validator vote submission"
    );
}

#[when("validator 2 OCOMP worker restarts and completes both pinned quorums")]
fn validator_two_worker_restarts_and_completes_dynamic_quorums(world: &mut World) {
    let requests = world.state.ocomp_dynamic_job_requests.clone();
    assert_eq!(requests.len(), 2, "job A and job B requests");
    let primary = world.validators.primary_port();
    let job_ids = requests
        .iter()
        .map(|request| {
            dynamic_job_record(world, request)
                .finalized
                .expect("dynamic finalized intent before validator-2 restart")
                .job_id
        })
        .collect::<Vec<_>>();
    let initial_accountability = job_ids
        .iter()
        .copied()
        .map(|job_id| {
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id)
                .expect("dynamic accountability before validator-2 restart")
        })
        .collect::<Vec<_>>();
    assert_eq!(initial_accountability[0].slot_validator_indexes.len(), 2);
    assert_eq!(initial_accountability[1].slot_validator_indexes.len(), 3);
    let finalized_before_restart = world
        .rpc
        .finalized(primary)
        .expect("finalized height before validator-2 Worker restart");

    world
        .ocomp
        .restart_worker(2, 0)
        .expect("restart validator-2 Worker for both pinned jobs");
    let deadline = Instant::now() + Duration::from_secs(180);
    let completed_accountability = loop {
        let observed = job_ids
            .iter()
            .copied()
            .map(|job_id| {
                world
                    .rpc
                    .finalized_ocomp_vote_accountability_on(primary, job_id)
            })
            .collect::<Vec<_>>();
        let ready = observed
            .iter()
            .enumerate()
            .all(|(ordinal, accountability)| {
                accountability.as_ref().is_some_and(|accountability| {
                    accountability.quorum_result_digest.is_some()
                        && accountability.slot_validator_indexes.len() == [3, 4][ordinal]
                        && initial_accountability[ordinal]
                            .slot_validator_indexes
                            .iter()
                            .all(|index| accountability.slot_validator_indexes.contains(index))
                })
            });
        if ready {
            break observed
                .into_iter()
                .map(|value| value.expect("checked completed accountability"))
                .collect::<Vec<_>>();
        }
        assert!(
            Instant::now() < deadline,
            "validator-2 did not complete both historical/current snapshot quorums"
        );
        sleep(Duration::from_millis(250));
    };

    let finalized_after_votes = world
        .rpc
        .finalized(primary)
        .expect("finalized height after validator-2 votes");
    for (ordinal, job_id) in job_ids.iter().copied().enumerate() {
        let vote = finalized_vote_for_delegate_on_job(
            world,
            finalized_before_restart.saturating_add(1),
            finalized_after_votes,
            2,
            job_id,
        )
        .unwrap_or_else(|| panic!("validator-2 public vote for job {job_id:#x}"));
        let participant_index = accountability_slot_for_vote(
            &completed_accountability[ordinal],
            &vote,
        )
        .unwrap_or_else(|| {
            panic!("validator-2 signature is absent from accountability for job {job_id:#x}")
        });
        assert!(
            !initial_accountability[ordinal]
                .slot_validator_indexes
                .contains(&participant_index),
            "validator-2 must populate a new canonical snapshot slot"
        );
        assert!(
            dynamic_vote_submission_path(world, 2, job_id).is_file(),
            "validator-2 must retain a durable vote submission for job {job_id:#x}"
        );
    }
    world.state.ocomp_dynamic_vote_slots = completed_accountability
        .iter()
        .map(|accountability| accountability.slot_validator_indexes.clone())
        .collect();
}

#[then("the FullNode result for job A matches the canonical quorum result")]
fn full_node_job_a_result_matches_quorum(world: &mut World) {
    let request = world
        .state
        .ocomp_dynamic_job_requests
        .first()
        .expect("job A request");
    let record = dynamic_job_record(world, request);
    let job_id = record
        .finalized
        .as_ref()
        .expect("job A finalized intent")
        .job_id;
    let encoded = std::fs::read(full_node_local_result_path(world, job_id))
        .expect("read persisted FullNode job A result after promotion");
    let result = LysisResultV1::decode_canonical(&encoded, &poc_schema_limits())
        .expect("decode persisted FullNode job A result");
    let local_digest = result
        .result_digest(&poc_schema_limits())
        .expect("validate persisted FullNode job A result");
    let accountability = world
        .rpc
        .finalized_ocomp_vote_accountability_on(world.validators.primary_port(), job_id)
        .expect("job A quorum accountability");
    assert_eq!(accountability.quorum_result_digest, Some(local_digest));
}

#[then("both deadlines record validator 3 missing with one soft penalty and all five validators stay live")]
fn dynamic_deadlines_preserve_active_membership_with_one_soft_penalty(world: &mut World) {
    // The full 43,200-block recovery/possible-jail acceptance is a separate lane.
    assert_eq!(world.validators.size(), 4, "four founders plus one joiner");
    let ports = dynamic_deadline_ports(
        world.validators.committee_ports(),
        world.validators.http_port(world.validators.joiner_index()),
    )
    .expect("five distinct expected observers");
    let members: Vec<Address> = (0..5)
        .map(|index| {
            let key = world
                .validators
                .get(index)
                .evm_key()
                .expect("validator identity");
            eth::address_of(&key).expect("derive public validator identity")
        })
        .collect();
    let victim = members[3];
    let owned: Vec<_> = (0..5)
        .map(|index| {
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("every expected validator and enclave must be owned and live")
        })
        .collect();
    let requests = world.state.ocomp_dynamic_job_requests.clone();
    assert_eq!(requests.len(), 2, "job A and job B requests");
    let expected_slots = world.state.ocomp_dynamic_vote_slots.clone();
    assert_eq!(expected_slots.len(), 2, "job A and job B voting slots");
    let deadlines = [requests[0].deadline_height, requests[1].deadline_height];
    let recovery = deadlines[0]
        .checked_add(DYNAMIC_OCOMP_RECOVERY_BLOCKS)
        .expect("recovery height overflow");
    assert!(
        deadlines[0] > 0 && deadlines[0] < deadlines[1] && deadlines[1] < recovery,
        "two ordered deadlines must lie in one recovery window"
    );
    let primary = world.validators.primary_port();
    let baseline_height = world
        .rpc
        .finalized_result(primary)
        .expect("finalized completed quorums");
    let records: Vec<_> = requests
        .iter()
        .map(|request| {
            world
                .rpc
                .ocomp_job_record_at_on(primary, request.intent_id, baseline_height)
                .expect("pinned dynamic job identity")
        })
        .collect();
    let job_ids: [B256; 2] = std::array::from_fn(|ordinal| {
        records[ordinal]
            .finalized
            .as_ref()
            .expect("finalized dynamic intent")
            .job_id
    });
    assert_ne!(job_ids[0], job_ids[1], "two different dynamic jobs");
    let baselines: Vec<_> = job_ids
        .iter()
        .map(|&job_id| {
            world
                .rpc
                .ocomp_vote_accountability_at_on(primary, job_id, baseline_height)
                .expect("retain completed quorum before the deadline wait")
        })
        .collect();
    for ordinal in 0..2 {
        assert_eq!(baselines[ordinal].job_id, job_ids[ordinal]);
        assert_eq!(
            baselines[ordinal].result_validator_set_epoch,
            records[ordinal].intent.result_validator_set_epoch
        );
        assert_eq!(
            baselines[ordinal].result_committee_set_hash,
            records[ordinal].intent.result_committee_set_hash
        );
        assert_eq!(
            baselines[ordinal].result_ocomp_binding_hash,
            records[ordinal].intent.result_ocomp_binding_hash
        );
    }
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("all-five fresh target")
        .max(
            deadlines[1]
                .checked_add(1)
                .expect("deadline height overflow"),
        );
    assert!(
        target < recovery,
        "fresh target already crossed the separate recovery gate"
    );
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadlines_armed", "ports": ports, "members": members,
        "owned_pids": owned, "baseline_height": baseline_height, "baselines": baselines,
        "job_ids": job_ids, "deadlines": deadlines, "target": target,
    }));
    // Preserve the original 900 x 2s allowance, now requiring every expected peer.
    let wait_deadline = Instant::now() + Duration::from_secs(900 * 2);
    let final_checkpoint = loop {
        dynamic_deadline_assert_live(world, &owned);
        let observations: Vec<_> = ports
            .iter()
            .map(|&port| world.rpc.finalized_result(port))
            .collect();
        assert!(
            observations.iter().all(|height| match height {
                Ok(height) => *height < recovery,
                Err(_) => true, // transient RPC failure still cannot satisfy the barrier below
            }),
            "an observer crossed the separate recovery gate"
        );
        if observations
            .iter()
            .all(|height| height.as_ref().is_ok_and(|height| *height >= target))
        {
            let height = observations
                .iter()
                .map(|height| *height.as_ref().expect("all finalities present"))
                .min()
                .expect("five finalities");
            assert!(
                height < recovery,
                "dynamic deadline proof crossed the separate recovery gate"
            );
            break dynamic_deadline_checkpoint(world, &ports, height)
                .expect("all-five common finalized hash/root");
        }
        assert!(
            Instant::now() < wait_deadline,
            "all five validators did not finalize dynamic deadlines: {observations:?}"
        );
        sleep(Duration::from_secs(2));
    };
    let checkpoints = deadlines.map(|height| {
        dynamic_deadline_checkpoint(world, &ports, height).expect("canonical closing block")
    });
    let heights = [
        deadlines[0] - 1,
        deadlines[0],
        deadlines[1] - 1,
        deadlines[1],
        final_checkpoint.height,
    ];
    let states: Vec<_> = heights
        .iter()
        .map(|&height| {
            let checkpoint = dynamic_deadline_checkpoint(world, &ports, height)
                .expect("pinned accounting checkpoint");
            let observed: Vec<_> = ports
                .iter()
                .map(|&port| {
                    dynamic_deadline_account(world, port, victim, height)
                        .expect("pinned soft-penalty accounting")
                })
                .collect();
            world.state.restart_observations.push(serde_json::json!({
                "phase": "ocomp_dynamic_deadline_accounting", "height": height,
                "block_hash": checkpoint.block_hash, "state_root": checkpoint.state_root,
                "ports": ports, "accounts": observed,
            }));
            assert!(
                observed.iter().all(|account| account == &observed[0]),
                "all-five accounting disagreement"
            );
            assert_eq!(
                dynamic_deadline_checkpoint(world, &ports, height).unwrap(),
                checkpoint
            );
            observed[0].clone()
        })
        .collect();
    let states: [DynamicDeadlineAccount; 5] = states
        .try_into()
        .expect("five pinned accounting observations");
    let mut agreed_events = None;
    for &port in &ports {
        let logs = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": crate::internal::addresses::WWD_ADDR,
                "fromBlock": format!("0x{:x}", requests[0].request_height),
                "toBlock": format!("0x{:x}", final_checkpoint.height),
                "topics": [eth::IMetadosis::OcompVoteMissed::SIGNATURE_HASH],
            }]),
        )
        .expect("finalized canonical OcompVoteMissed events");
        world.state.restart_observations.push(serde_json::json!({
            "phase": "ocomp_dynamic_deadline_events", "port": port, "logs": logs,
            "through_height": final_checkpoint.height,
        }));
        let events = dynamic_deadline_decode_events(&logs, victim, job_ids, checkpoints)
            .expect("exact first/repeat OCOMP event identities");
        for event in &events {
            let receipt = eth::raw_json_result(
                &world.rpc.url(port),
                "eth_getTransactionReceipt",
                serde_json::json!([event.transaction_hash]),
            )
            .expect("canonical OcompLifecycleBegin receipt RPC");
            world.state.restart_observations.push(serde_json::json!({
                "phase": "ocomp_dynamic_deadline_receipt", "port": port,
                "event": event, "receipt": receipt,
            }));
            dynamic_deadline_validate_receipt(&receipt, event)
                .expect("successful canonical system receipt contains the exact miss event");
        }
        dynamic_deadline_validate_penalties(
            &states,
            &events,
            &members,
            deadlines,
            final_checkpoint.height,
        )
        .expect("one bonded-only slash, fixed recovery window, and unchanged ACTIVE membership");
        if let Some(ref expected) = agreed_events {
            assert_eq!(&events, expected, "all-five event disagreement");
        } else {
            agreed_events = Some(events);
        }
    }
    for (ordinal, request) in requests.iter().enumerate() {
        let mut agreed = None;
        for &port in &ports {
            let closed = world
                .rpc
                .ocomp_vote_accountability_at_on(port, job_ids[ordinal], request.deadline_height)
                .expect("accountability at exact closing block");
            dynamic_deadline_validate_accountability(
                &closed,
                &baselines[ordinal],
                &expected_slots[ordinal],
                request.deadline_height,
                [(4, 3), (5, 4)][ordinal],
            )
            .expect("pinned historical quorum and exact singleton missing bitmap");
            let record = world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, final_checkpoint.height)
                .expect("finalized dynamic job remains available");
            assert_eq!(
                record, records[ordinal],
                "closing mutated the completed job/result/binding"
            );
            assert_eq!(
                world
                    .rpc
                    .ocomp_vote_accountability_at_on(
                        port,
                        job_ids[ordinal],
                        final_checkpoint.height
                    )
                    .expect("post-deadline accountability"),
                closed,
                "closed accountability changed later"
            );
            if let Some(ref first) = agreed {
                assert_eq!(
                    &closed, first,
                    "all-five closed accountability disagreement"
                );
            } else {
                agreed = Some(closed);
            }
        }
        world.state.restart_observations.push(serde_json::json!({
            "phase": "ocomp_dynamic_deadline_accountability", "job_id": job_ids[ordinal],
            "accountability": agreed, "ports": ports,
        }));
    }
    dynamic_deadline_assert_live(world, &owned);
    assert_eq!(
        dynamic_deadline_checkpoint(world, &ports, final_checkpoint.height).unwrap(),
        final_checkpoint
    );
    dynamic_deadline_assert_live(world, &owned);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadlines_verified", "ports": ports, "owned_pids": owned,
        "height": final_checkpoint.height, "block_hash": final_checkpoint.block_hash,
        "state_root": final_checkpoint.state_root, "recovery_deadline": recovery,
    }));
}

// Canonical policy: crates/system/validatorset/src/runtime.rs. Not a test override.
const DYNAMIC_OCOMP_RECOVERY_BLOCKS: u64 = 43_200;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct DynamicDeadlineAccount {
    bonded: U256,
    mirrored: U256,
    total_staked: U256,
    staking_balance: U256,
    status: u8,
    ordinary_slash_count: u64,
    ocomp_miss_count: u64,
    ocomp_recovery_deadline: u64,
    active: Vec<Address>,
    participants: Vec<Address>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct DynamicDeadlineMiss {
    validator: Address,
    job_id: B256,
    miss_count: u64,
    slashed_bonded: U256,
    recovery_deadline: u64,
    first_in_window: bool,
    height: u64,
    block_hash: B256,
    transaction_hash: B256,
    log_index: u64,
}

fn dynamic_deadline_ports(mut founders: Vec<u16>, joiner: u16) -> eyre::Result<Vec<u16>> {
    ensure!(founders.len() == 4, "four expected founder observers");
    founders.push(joiner);
    ensure!(
        founders
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == 5,
        "five distinct observers including validator 3 and the joiner"
    );
    Ok(founders)
}

fn dynamic_deadline_assert_live(world: &mut World, owned: &[(u32, u32)]) {
    assert_eq!(owned.len(), 5, "five expected owned process pairs");
    for (index, expected) in owned.iter().enumerate() {
        assert_eq!(
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("dynamic deadline observer exited or is unobservable"),
            *expected,
            "dynamic deadline observer changed incarnation"
        );
    }
}

fn dynamic_deadline_checkpoint(
    world: &World,
    ports: &[u16],
    height: u64,
) -> eyre::Result<crate::world::rpc::FinalizedCheckpoint> {
    let observed = ports
        .iter()
        .map(|&port| {
            Ok((
                port,
                world.rpc.finalized_result(port)?,
                world.rpc.checkpoint_at(port, height)?,
            ))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    dynamic_deadline_validate_checkpoints(ports, height, &observed)
}

fn dynamic_deadline_validate_checkpoints(
    ports: &[u16],
    height: u64,
    observed: &[(u16, u64, crate::world::rpc::FinalizedCheckpoint)],
) -> eyre::Result<crate::world::rpc::FinalizedCheckpoint> {
    ensure!(
        ports.len() == 5
            && ports
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 5,
        "five distinct expected finalized observers required"
    );
    ensure!(
        observed.len() == ports.len(),
        "a finalized observer is missing"
    );
    let expected = observed[0].2;
    ensure!(expected.height == height, "wrong pinned checkpoint height");
    for (&port, &(actual_port, finalized, checkpoint)) in ports.iter().zip(observed) {
        ensure!(port == actual_port, "wrong finalized observer identity");
        ensure!(finalized >= height, "observer has not finalized h{height}");
        ensure!(
            checkpoint == expected,
            "finalized hash/root mismatch at h{height}"
        );
    }
    Ok(expected)
}

fn dynamic_deadline_account(
    world: &World,
    port: u16,
    victim: Address,
    height: u64,
) -> eyre::Result<DynamicDeadlineAccount> {
    use crate::internal::addresses::{STK_ADDR, VS_ADDR};
    let url = world.rpc.url(port);
    let record = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::validatorByAddressCall { addr: victim },
        height,
    )
    .map_err(|e| eyre!(e))?;
    ensure!(
        record.validatorAddress == victim,
        "wrong validator accounting identity"
    );
    let mut active = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    let mut participants = eth::read_call_at_result(
        &url,
        VS_ADDR,
        &eth::IValidatorSet::getActiveConsensusSetCall {},
        height,
    )
    .map_err(|e| eyre!(e))?;
    active.sort_unstable();
    participants.sort_unstable();
    let recovery_word = |base: u64| -> eyre::Result<u64> {
        // ValidatorSet schema slots 61/62 use this same production mapping helper.
        let slot = outbe_primitives::storage::StorageKey::mapping_slot(&victim, U256::from(base));
        let value = eth::raw_json_result(
            &url,
            "eth_getStorageAt",
            serde_json::json!([VS_ADDR, format!("0x{slot:x}"), format!("0x{height:x}")]),
        )?;
        dynamic_deadline_storage_u64(&value)
    };
    Ok(DynamicDeadlineAccount {
        bonded: eth::read_call_at_result(
            &url,
            STK_ADDR,
            &eth::IStaking::getStakeCall { validator: victim },
            height,
        )
        .map_err(|e| eyre!(e))?,
        mirrored: record.stake,
        total_staked: eth::read_call_at_result(
            &url,
            STK_ADDR,
            &eth::IStaking::getTotalStakedCall {},
            height,
        )
        .map_err(|e| eyre!(e))?,
        staking_balance: serde_json::from_value(
            eth::raw_json_result(
                &url,
                "eth_getBalance",
                serde_json::json!([STK_ADDR, format!("0x{height:x}")]),
            )
            .map_err(|e| eyre!(e))?,
        )?,
        status: record.status,
        ordinary_slash_count: record.slashCount,
        ocomp_miss_count: recovery_word(61)?,
        ocomp_recovery_deadline: recovery_word(62)?,
        active,
        participants,
    })
}

fn dynamic_deadline_storage_u64(value: &serde_json::Value) -> eyre::Result<u64> {
    // eth_getStorageAt returns a complete 32-byte word, not an optional counter.
    let word: B256 = serde_json::from_value(value.clone())?;
    Ok(U256::from_be_bytes(word.0).try_into()?)
}

fn dynamic_deadline_decode_events(
    logs: &serde_json::Value,
    victim: Address,
    jobs: [B256; 2],
    checkpoints: [crate::world::rpc::FinalizedCheckpoint; 2],
) -> eyre::Result<[DynamicDeadlineMiss; 2]> {
    let rows = logs
        .as_array()
        .ok_or_else(|| eyre!("miss events are not an array"))?;
    ensure!(
        rows.len() == 2,
        "exactly two canonical OcompVoteMissed events required"
    );
    let quantity = |value: &serde_json::Value| -> eyre::Result<u64> {
        let word: U256 = serde_json::from_value(value.clone())?;
        Ok(word.try_into()?)
    };
    let mut events = Vec::new();
    for row in rows {
        let source: Address = serde_json::from_value(row["address"].clone())?;
        ensure!(
            source == crate::internal::addresses::WWD_ADDR,
            "wrong OCOMP event emitter"
        );
        ensure!(
            row["removed"].as_bool() == Some(false),
            "removed or unqualified OCOMP event"
        );
        let topics: Vec<B256> = serde_json::from_value(row["topics"].clone())?;
        let data: Bytes = serde_json::from_value(row["data"].clone())?;
        ensure!(
            topics.len() == 3 && data.len() == 128,
            "malformed OcompVoteMissed shape"
        );
        let event = eth::IMetadosis::OcompVoteMissed::decode_raw_log_validate(
            topics.iter().copied(),
            &data,
        )?;
        let canonical = event.encode_log_data();
        ensure!(
            canonical.topics() == topics.as_slice() && canonical.data == data,
            "noncanonical OcompVoteMissed ABI encoding"
        );
        events.push(DynamicDeadlineMiss {
            validator: event.validator,
            job_id: event.jobId,
            miss_count: event.missCount,
            slashed_bonded: event.slashedBonded,
            recovery_deadline: event.recoveryDeadline,
            first_in_window: event.firstInWindow,
            height: quantity(&row["blockNumber"])?,
            block_hash: serde_json::from_value(row["blockHash"].clone())?,
            transaction_hash: serde_json::from_value(row["transactionHash"].clone())?,
            log_index: quantity(&row["logIndex"])?,
        });
    }
    events.sort_by_key(|event| (event.height, event.log_index));
    for (ordinal, event) in events.iter().enumerate() {
        ensure!(
            event.validator == victim && event.job_id == jobs[ordinal],
            "wrong missing validator/job identity"
        );
        ensure!(
            event.height == checkpoints[ordinal].height
                && event.block_hash == checkpoints[ordinal].block_hash,
            "miss event is not in the exact canonical closing block"
        );
    }
    events
        .try_into()
        .map_err(|_| eyre!("two miss events required"))
}

fn dynamic_deadline_validate_receipt(
    receipt: &serde_json::Value,
    event: &DynamicDeadlineMiss,
) -> eyre::Result<()> {
    let quantity = |value: &serde_json::Value| -> eyre::Result<u64> {
        let word: U256 = serde_json::from_value(value.clone())?;
        Ok(word.try_into()?)
    };
    let hash = |value: &serde_json::Value| -> eyre::Result<B256> {
        Ok(serde_json::from_value(value.clone())?)
    };
    ensure!(
        quantity(&receipt["status"])? == 1
            && quantity(&receipt["blockNumber"])? == event.height
            && hash(&receipt["blockHash"])? == event.block_hash
            && hash(&receipt["transactionHash"])? == event.transaction_hash
            && event.transaction_hash != B256::ZERO,
        "missing, reverted, or foreign OCOMP system receipt"
    );
    let expected = eth::IMetadosis::OcompVoteMissed {
        validator: event.validator,
        jobId: event.job_id,
        missCount: event.miss_count,
        slashedBonded: event.slashed_bonded,
        recoveryDeadline: event.recovery_deadline,
        firstInWindow: event.first_in_window,
    }
    .encode_log_data();
    let rows = receipt["logs"]
        .as_array()
        .ok_or_else(|| eyre!("receipt has no logs"))?;
    let mut matched = 0;
    for row in rows {
        if quantity(&row["logIndex"])? != event.log_index {
            continue;
        }
        matched += 1;
        let address: Address = serde_json::from_value(row["address"].clone())?;
        let topics: Vec<B256> = serde_json::from_value(row["topics"].clone())?;
        let data: Bytes = serde_json::from_value(row["data"].clone())?;
        ensure!(
            address == crate::internal::addresses::WWD_ADDR
                && topics.as_slice() == expected.topics()
                && data == expected.data
                && row["removed"].as_bool() == Some(false)
                && quantity(&row["blockNumber"])? == event.height
                && hash(&row["blockHash"])? == event.block_hash
                && hash(&row["transactionHash"])? == event.transaction_hash,
            "receipt does not contain the exact canonical OcompVoteMissed log"
        );
    }
    ensure!(matched == 1, "receipt must include the exact miss log once");
    Ok(())
}

fn dynamic_deadline_validate_penalties(
    states: &[DynamicDeadlineAccount; 5],
    events: &[DynamicDeadlineMiss; 2],
    members: &[Address],
    deadlines: [u64; 2],
    final_height: u64,
) -> eyre::Result<()> {
    ensure!(members.len() == 5, "five expected identities required");
    let mut sorted_members = members.to_vec();
    sorted_members.sort_unstable();
    ensure!(
        sorted_members.windows(2).all(|pair| pair[0] != pair[1]),
        "duplicate expected validator identity"
    );
    let recovery = deadlines[0]
        .checked_add(DYNAMIC_OCOMP_RECOVERY_BLOCKS)
        .ok_or_else(|| eyre!("recovery deadline overflow"))?;
    ensure!(
        deadlines[0] > 0
            && deadlines[0] < deadlines[1]
            && deadlines[1] < final_height
            && final_height < recovery,
        "observations must pass both deadlines but precede recovery expiry"
    );
    let miss_counts = [0, 1, 1, 2, 2];
    let recovery_deadlines = [0, recovery, recovery, recovery, recovery];
    for (ordinal, state) in states.iter().enumerate() {
        ensure!(
            state.ocomp_miss_count == miss_counts[ordinal]
                && state.ocomp_recovery_deadline == recovery_deadlines[ordinal],
            "durable OCOMP miss count or fixed recovery deadline is incorrect"
        );
        ensure!(
            state.status == 2
                && state.active == sorted_members
                && state.participants == sorted_members,
            "soft penalty must preserve all five ACTIVE consensus participants"
        );
        ensure!(
            state.bonded == state.mirrored,
            "bonded/mirrored stake mismatch"
        );
    }
    let slash = states[0].bonded / U256::from(10);
    ensure!(
        !slash.is_zero(),
        "funded fixture must exercise a positive first-miss slash"
    );
    for (ordinal, event) in events.iter().enumerate() {
        ensure!(
            event.validator == members[3] && event.height == deadlines[ordinal],
            "wrong miss identity/height"
        );
        ensure!(
            event.miss_count == (ordinal + 1) as u64 && event.first_in_window == (ordinal == 0),
            "wrong first/repeat miss count or flag"
        );
        ensure!(
            event.recovery_deadline == recovery,
            "OCOMP recovery deadline moved"
        );
        ensure!(
            event.slashed_bonded == if ordinal == 0 { slash } else { U256::ZERO },
            "OCOMP must slash bonded stake exactly once"
        );
    }
    let mut expected = states[0].clone();
    expected.bonded = expected
        .bonded
        .checked_sub(slash)
        .ok_or_else(|| eyre!("bonded slash underflow"))?;
    expected.mirrored = expected.bonded;
    expected.total_staked = expected
        .total_staked
        .checked_sub(slash)
        .ok_or_else(|| eyre!("total stake underflow"))?;
    expected.staking_balance = expected
        .staking_balance
        .checked_sub(slash)
        .ok_or_else(|| eyre!("staking balance underflow"))?;
    for (ordinal, state) in states.iter().enumerate().skip(1) {
        expected.ocomp_miss_count = miss_counts[ordinal];
        expected.ocomp_recovery_deadline = recovery_deadlines[ordinal];
        ensure!(state == &expected,
            "first/repeat/post-deadline accounting changed beyond one bonded-only slash (including ordinary slash count)");
    }
    Ok(())
}

fn dynamic_deadline_validate_accountability(
    closed: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    baseline: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    slots: &[u16],
    deadline: u64,
    membership: (u16, u16),
) -> eyre::Result<()> {
    ensure!(
        (closed.member_count, closed.quorum_threshold) == membership
            && (baseline.member_count, baseline.quorum_threshold) == membership,
        "historical membership/quorum changed"
    );
    ensure!(
        closed.job_id == baseline.job_id
            && closed.result_validator_set_epoch == baseline.result_validator_set_epoch
            && closed.result_committee_set_hash == baseline.result_committee_set_hash
            && closed.result_ocomp_binding_hash == baseline.result_ocomp_binding_hash,
        "historical job/binding changed"
    );
    ensure!(
        baseline.quorum_result_digest.is_some()
            && closed.quorum_result_digest == baseline.quorum_result_digest
            && closed.quorum_height == baseline.quorum_height
            && closed.quorum_signer_bitmap == baseline.quorum_signer_bitmap,
        "completed quorum/result changed at close"
    );
    ensure!(
        closed.slot_validator_indexes == slots
            && baseline.slot_validator_indexes == slots
            && closed.slot_first_signatures == baseline.slot_first_signatures,
        "accepted votes changed at close"
    );
    ensure!(
        slots.len() == usize::from(membership.1)
            && slots.windows(2).all(|pair| pair[0] < pair[1])
            && slots.iter().all(|index| *index < membership.0),
        "invalid pinned quorum slots"
    );
    ensure!(
        closed.closed_height == Some(deadline),
        "wrong accountability closing height"
    );
    let missing: Vec<_> = (0..closed.member_count)
        .filter(|index| !slots.contains(index))
        .collect();
    ensure!(
        missing.len() == 1,
        "exactly one historical participant must be missing"
    );
    ensure!(
        closed.missing_bitmap
            == Some(singleton_participant_bitmap(
                closed.member_count,
                missing[0]
            )),
        "wrong missing bitmap"
    );
    Ok(())
}

#[then("the fresh capacity day is created in FORMING by finalized block 1")]
fn fresh_capacity_day_is_created_in_forming(world: &mut World) {
    let worldwide_day = fresh_metadosis_wwd(world);
    let deadline = Instant::now() + Duration::from_secs(120);
    let (started, state, finalized_points) = loop {
        let points = finalized_points_at_common_height(world, 1);
        let common_height = points[0].block_number;
        let started = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_metadosis_wwd_started_on(port, worldwide_day)
            })
            .collect::<Vec<_>>();
        let states = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .metadosis_wwd_state_at(port, worldwide_day, common_height)
            })
            .collect::<Vec<_>>();
        if started.iter().all(Option::is_some)
            && states.iter().all(Option::is_some)
            && states
                .iter()
                .all(|candidate| candidate.as_ref() == states[0].as_ref())
            && states[0].as_ref().is_some_and(|state| state.status == 0)
        {
            let first_started = started[0].clone().expect("finalized WWD started event");
            assert!(
                started
                    .iter()
                    .all(|candidate| candidate.as_ref() == Some(&first_started)),
                "validators expose different finalized WorldwideDayStarted events"
            );
            break (
                first_started,
                states[0].clone().expect("finalized FORMING state"),
                points,
            );
        }
        assert!(
            Instant::now() < deadline,
            "fresh Metadosis day was not created in finalized FORMING state"
        );
        sleep(Duration::from_millis(250));
    };

    assert_eq!(
        started.block_number, 1,
        "fresh WWD must be created at block 1"
    );
    assert_eq!(started.worldwide_day, worldwide_day);
    assert_eq!(started.forming_start, state.forming_start);
    assert_eq!(started.forming_end, state.forming_end);
    assert_eq!(started.lookback_end, state.lookback_end);
    assert_eq!(started.offering_end, state.offering_end);
    assert_eq!(started.scheduled_process_time, state.scheduled_process_time);
    let genesis_path = world.ocomp.canonical_chain_manifest_path();
    let genesis: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&genesis_path).expect("read fresh Metadosis genesis"),
    )
    .expect("decode fresh Metadosis genesis");
    let protocol_constants = GenesisProtocolParametersV1::from_genesis(&genesis)
        .expect("read immutable fresh Metadosis protocol constants");
    assert_eq!(
        state.forming_end - state.forming_start,
        protocol_constants.metadosis_forming_period_seconds,
        "fresh process evidence must use the immutable genesis FORMING duration"
    );
    assert_eq!(
        protocol_constants.metadosis_forming_period_seconds,
        outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
        "fresh WWD fixture must preserve the canonical 50-hour VWAP window"
    );
    assert_eq!(
        state.lookback_end - state.forming_end,
        protocol_constants.metadosis_lookback_delay_seconds,
        "fresh process evidence must use the immutable genesis LOOKBACK duration"
    );
    assert_eq!(
        state.offering_end - state.lookback_end,
        protocol_constants.metadosis_offering_period_seconds,
        "fresh process evidence must use the immutable genesis OFFERING duration"
    );
    assert_eq!(
        state.scheduled_process_time - state.offering_end,
        protocol_constants.metadosis_waiting_period_seconds,
        "fresh process evidence must use the immutable genesis WAITING duration"
    );
    let requested_initial_timestamp = world
        .state
        .metadosis_fresh_initial_timestamp
        .expect("fresh Metadosis initial logical timestamp");
    let initial_timestamps = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .block_timestamp(port, started.block_number)
                .expect("canonical block-1 timestamp")
        })
        .collect::<Vec<_>>();
    assert!(
        initial_timestamps
            .iter()
            .all(|timestamp| *timestamp == initial_timestamps[0]),
        "validators expose different block-1 timestamps"
    );
    let initial_timestamp = initial_timestamps[0];
    assert!(
        initial_timestamp >= state.forming_start && initial_timestamp < state.forming_end,
        "initial logical time must place the runtime-created WWD inside FORMING"
    );
    assert!(
        initial_timestamp >= requested_initial_timestamp,
        "block-1 timestamp preceded the requested fresh logical time"
    );
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp),
        WorldwideDay::new(worldwide_day),
        "runtime block-1 UTC+14 mapping selected a different WWD"
    );
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp.saturating_sub(14 * 3_600)),
        WorldwideDay::new(worldwide_day),
        "runtime block-1 UTC date mapping selected a different WWD"
    );
    let genesis_hash = common_block_hash(world, 0);
    assert_eq!(
        common_block_hash(world, started.block_number),
        started.block_hash,
        "WorldwideDayStarted is not bound to canonical block 1"
    );
    assert!(
        finalized_points
            .iter()
            .all(|point| point.block_number >= started.block_number),
        "all validators must finalize the block-1 WWD creation"
    );
    let unknown_status_revert_validator_count = u8::try_from(
        world
            .validators
            .committee_ports()
            .into_iter()
            .filter(|port| {
                world
                    .rpc
                    .metadosis_unknown_status_reverts_at(*port, u8::MAX, started.block_number)
                    == Some(true)
            })
            .count(),
    )
    .expect("validator count fits u8");
    assert_eq!(
        unknown_status_revert_validator_count, 4,
        "unknown WwdStatus must revert on all validators at canonical block 1"
    );
    world.state.metadosis_fresh_lifecycle_observation =
        Some(MetadosisFreshLifecycleObservationV1 {
            worldwide_day,
            genesis_hash,
            initial_timestamp,
            initial_unix_time_offset_secs: world
                .state
                .metadosis_fresh_initial_unix_time_offset_secs
                .expect("fresh Metadosis initial logical offset"),
            forming_start: state.forming_start,
            forming_end: state.forming_end,
            lookback_end: state.lookback_end,
            offering_end: state.offering_end,
            scheduled_process_time: state.scheduled_process_time,
            started,
            status_changes: Vec::new(),
            time_control_epochs: Vec::new(),
            created_validator_count: 4,
            unknown_status_revert_validator_count,
            offering_validator_count: 0,
            ready_validator_count: 0,
            completed_validator_count: 0,
        });
}

#[when("the committee logical clock reaches the fresh capacity OFFERING window")]
fn committee_clock_reaches_fresh_capacity_offering(world: &mut World) {
    let target = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("fresh Metadosis creation evidence")
        .forming_end
        .saturating_add(1);
    advance_fresh_metadosis_time(world, target, &[(0, 1), (1, 2)], 2);
}

#[then("the same fresh capacity day advances through LOOKBACK to OFFERING")]
fn fresh_capacity_day_advances_to_offering(world: &mut World) {
    let lifecycle = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("fresh Metadosis lifecycle evidence");
    assert_eq!(lifecycle.offering_validator_count, 4);
    assert_eq!(
        lifecycle
            .status_changes
            .iter()
            .map(|edge| (edge.old_status, edge.new_status))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 2)]
    );
}

#[when("the committee logical clock reaches the fresh capacity processing time")]
fn committee_clock_reaches_fresh_capacity_processing(world: &mut World) {
    // These three artifact scenarios require all four independent outputs, not
    // merely a quorum. Keep workers held until every node has dispatched the
    // exact exported job; unrelated capacity and fault scenarios use their own
    // processing steps and retain their original scheduling.
    assert!(!world.state.ocomp_pending_v1_workers_held);
    for validator_index in 0..4 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 0,
            })
            .expect("hold each V1 worker before the request can be created");
    }
    world.state.ocomp_pending_v1_workers_held = true;
    let scheduled_process_time = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("fresh Metadosis creation evidence")
        .scheduled_process_time;
    // ProtocolCycle advances WWD state and processes one READY candidate at the
    // first genesis-configured aligned slot at or after the processing time.
    let target = first_protocol_cycle_at_or_after(world, scheduled_process_time);
    advance_fresh_metadosis_time(world, target, &[(0, 1), (1, 2), (2, 3), (3, 4)], 8);
}

#[when("the committee reaches fresh capacity processing with V1 workers held for the test-only V2 activation")]
fn committee_processing_holds_v1_for_test_successor(world: &mut World) {
    committee_clock_reaches_fresh_capacity_processing(world);
}

fn first_protocol_cycle_at_or_after(world: &World, timestamp: u64) -> u64 {
    let genesis_path = world.ocomp.canonical_chain_manifest_path();
    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).expect("read ProtocolCycle genesis"))
            .expect("decode ProtocolCycle genesis");
    let interval = GenesisProtocolParametersV1::from_genesis(&genesis)
        .expect("read immutable ProtocolCycle interval")
        .metadosis_advance_interval_seconds;
    first_protocol_cycle_at_or_after_interval(timestamp, interval)
}

fn first_protocol_cycle_at_or_after_interval(timestamp: u64, interval: u64) -> u64 {
    assert!(interval != 0, "ProtocolCycle interval must be non-zero");
    timestamp
        .checked_add(interval - 1)
        .and_then(|rounded| rounded.checked_div(interval))
        .and_then(|slot| slot.checked_mul(interval))
        .and_then(|slot| slot.checked_add(1))
        .expect("first aligned ProtocolCycle at or after Metadosis processing time")
}

#[then("the same fresh capacity day advances through WAITING and READY")]
fn fresh_capacity_day_advances_through_ready(world: &mut World) {
    let lifecycle = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("fresh Metadosis lifecycle evidence");
    assert_eq!(lifecycle.ready_validator_count, 4);
    assert_eq!(
        lifecycle
            .status_changes
            .iter()
            .map(|edge| (edge.old_status, edge.new_status))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 2), (2, 3), (3, 4)]
    );
}

#[then("the fresh OCOMP domains retain their authenticated workers across the time changes")]
fn fresh_domains_retain_authenticated_workers(world: &mut World) {
    for validator_index in 0..4_u8 {
        world
            .ocomp
            .ensure_worker_alive(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!(
                    "validator-{validator_index} worker did not survive the committee time changes: {error:#}"
                )
            });
    }
    let records = world.ocomp.process_records();
    for validator_index in 0..4_u8 {
        assert_eq!(
            records
                .iter()
                .filter(|record| {
                    record.validator_index == Some(validator_index)
                        && record.role == OcompProcessRole::Worker
                        && record.worker_ordinal == Some(0)
                        && record.stopped_at_millis.is_none()
                })
                .count(),
            1,
            "validator-{validator_index} must retain one live authenticated worker"
        );
    }
}

/// How long the logical clock may stand still before the ratchet counts as
/// stalled. Bounding the wait by progress rather than by the distance jumped
/// keeps a loaded host from expiring a run that is still moving.
const RATCHET_STALL_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RestartBarrierState {
    lifecycle_observed: bool,
    publication_observed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestartBarrierDecision {
    Continue(RestartBarrierState),
    Complete,
    HistoricalLifecycleRequired(RestartBarrierState),
}

fn restart_barrier_decision(
    previous: RestartBarrierState,
    lifecycle_observed_now: bool,
    lifecycle_overshot_now: bool,
    publication_observed_now: bool,
) -> RestartBarrierDecision {
    let next = RestartBarrierState {
        lifecycle_observed: previous.lifecycle_observed || lifecycle_observed_now,
        publication_observed: previous.publication_observed || publication_observed_now,
    };
    if next.lifecycle_observed && next.publication_observed {
        RestartBarrierDecision::Complete
    } else if !next.lifecycle_observed && lifecycle_overshot_now {
        RestartBarrierDecision::HistoricalLifecycleRequired(next)
    } else {
        RestartBarrierDecision::Continue(next)
    }
}

const MAX_HISTORICAL_LIFECYCLE_SCAN_BLOCKS: u64 = 256;

fn historical_lifecycle_scan_heights(
    minimum_height: u64,
    current_height: u64,
) -> Result<Vec<u64>, String> {
    if current_height < minimum_height {
        return Err(format!(
            "current finalized height {current_height} precedes restart minimum {minimum_height}"
        ));
    }
    let count = current_height
        .checked_sub(minimum_height)
        .and_then(|distance| distance.checked_add(1))
        .ok_or_else(|| "historical lifecycle scan height arithmetic overflowed".to_string())?;
    if count > MAX_HISTORICAL_LIFECYCLE_SCAN_BLOCKS {
        return Err(format!(
            "historical lifecycle scan spans {count} blocks; maximum is {MAX_HISTORICAL_LIFECYCLE_SCAN_BLOCKS}"
        ));
    }
    Ok((minimum_height..=current_height).rev().collect())
}

fn fresh_wwd_lifecycle_overshot(expected_status: u8, observed: &[Option<u8>]) -> bool {
    let Some(status) = observed.first().copied().flatten() else {
        return false;
    };
    if !observed.iter().all(|candidate| *candidate == Some(status)) {
        return false;
    }
    match expected_status {
        2 => matches!(status, 3 | 4 | 6 | 7 | 8),
        8 => matches!(status, 6 | 7),
        _ => false,
    }
}

fn advance_fresh_metadosis_time(
    world: &mut World,
    requested_timestamp: u64,
    expected_edges: &[(u8, u8)],
    expected_persisted_status: u8,
) {
    let (offset, before_restart, minimum_height, pending_price_publication) =
        restart_committee_at_logical_time(world, requested_timestamp);
    let before_timestamp = before_restart[0].block_timestamp;
    let worldwide_day = fresh_metadosis_wwd(world);
    // The chain closes the gap one hour per block, so wait on progress rather
    // than on a budget derived from the distance: a loaded host slows block
    // production without stalling it.
    let mut deadline = Instant::now() + RATCHET_STALL_TIMEOUT;
    let mut last_timestamp = before_timestamp;
    let mut barrier = RestartBarrierState {
        lifecycle_observed: false,
        publication_observed: pending_price_publication.is_none(),
    };
    let mut lifecycle_observation = None;
    let (after_restart, changes) = loop {
        let points = finalized_points_at_common_height(world, minimum_height);
        let common_height = points[0].block_number;
        if points[0].block_timestamp > last_timestamp {
            last_timestamp = points[0].block_timestamp;
            deadline = Instant::now() + RATCHET_STALL_TIMEOUT;
        }
        let states = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .metadosis_wwd_state_at(port, worldwide_day, common_height)
            })
            .collect::<Vec<_>>();
        let changes = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_metadosis_wwd_status_changes_on(port, worldwide_day)
            })
            .collect::<Vec<_>>();
        let lifecycle_now = states.iter().all(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.status == expected_persisted_status)
        }) && changes.iter().all(Option::is_some);
        let matching_changes = if lifecycle_now {
            let first = changes[0]
                .clone()
                .expect("finalized Metadosis status changes");
            if first
                .iter()
                .map(|edge| (edge.old_status, edge.new_status))
                .eq(expected_edges.iter().copied())
                && changes
                    .iter()
                    .all(|candidate| candidate.as_ref() == Some(&first))
            {
                Some(first)
            } else {
                None
            }
        } else {
            None
        };
        let lifecycle_now = matching_changes.is_some();
        if lifecycle_observation.is_none() {
            if let Some(first) = matching_changes {
                lifecycle_observation = Some((points.clone(), first));
            }
        }
        let publication_now = if barrier.publication_observed {
            false
        } else {
            pending_price_publication.as_ref().is_some_and(|pending| {
                crate::features::price_oracle::observe_pending_publication(world, pending)
            })
        };
        let observed_statuses = states
            .iter()
            .map(|state| state.as_ref().map(|state| state.status))
            .collect::<Vec<_>>();
        match restart_barrier_decision(
            barrier,
            lifecycle_now,
            fresh_wwd_lifecycle_overshot(expected_persisted_status, &observed_statuses),
            publication_now,
        ) {
            RestartBarrierDecision::Complete => {
                break lifecycle_observation
                    .take()
                    .expect("completed restart barrier has a lifecycle observation")
            }
            RestartBarrierDecision::HistoricalLifecycleRequired(next) => {
                let observation = historical_lifecycle_observation(
                    world,
                    worldwide_day,
                    minimum_height,
                    common_height,
                    expected_persisted_status,
                    expected_edges,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "fresh Metadosis WWD passed status {expected_persisted_status}, and the bounded canonical history did not prove the required transition: statuses={observed_statuses:?}; {error}"
                    )
                });
                lifecycle_observation = Some(observation);
                barrier = RestartBarrierState {
                    lifecycle_observed: true,
                    publication_observed: next.publication_observed,
                };
                if barrier.publication_observed {
                    break lifecycle_observation
                        .take()
                        .expect("historical lifecycle proof completes the restart barrier");
                }
            }
            RestartBarrierDecision::Continue(next) => barrier = next,
        }
        assert!(
            Instant::now() < deadline,
            "fresh Metadosis WWD did not reach status {expected_persisted_status} with edges \
             {expected_edges:?}; observed statuses {observed:?} and edges {seen:?} at logical \
             timestamp {reached} (requested {requested_timestamp}); the drift ratchet made no \
             progress for {stall:?}",
            observed = states
                .iter()
                .map(|state| state.as_ref().map(|state| state.status))
                .collect::<Vec<_>>(),
            seen = changes[0].as_ref().map(|edges| edges
                .iter()
                .map(|edge| (edge.old_status, edge.new_status))
                .collect::<Vec<_>>()),
            reached = points[0].block_timestamp,
            stall = RATCHET_STALL_TIMEOUT,
        );
        sleep(Duration::from_millis(250));
    };
    let transition = changes
        .last()
        .expect("expected at least one Metadosis status change");
    let transition_timestamp = world
        .rpc
        .block_timestamp(world.validators.primary_port(), transition.block_number)
        .expect("canonical Metadosis transition block timestamp");
    let transition_floor = if expected_persisted_status == 8 {
        world
            .state
            .metadosis_fresh_lifecycle_observation
            .as_ref()
            .expect("fresh Metadosis lifecycle evidence")
            .scheduled_process_time
    } else {
        requested_timestamp
    };
    assert!(
        transition_timestamp >= transition_floor.saturating_sub(1),
        "Metadosis transition occurred before its canonical phase boundary"
    );

    let current_genesis_hash = common_block_hash(world, 0);
    let lifecycle = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_mut()
        .expect("fresh Metadosis lifecycle evidence");
    assert_eq!(lifecycle.genesis_hash, current_genesis_hash);
    lifecycle.status_changes = changes;
    lifecycle
        .time_control_epochs
        .push(MetadosisTimeControlEpochV1 {
            requested_timestamp,
            unix_time_offset_secs: offset,
            before_restart,
            after_restart,
        });
    if expected_persisted_status == 2 {
        lifecycle.offering_validator_count = 4;
    } else {
        lifecycle.ready_validator_count = 4;
    }
}

fn historical_lifecycle_observation(
    world: &World,
    worldwide_day: u32,
    minimum_height: u64,
    current_height: u64,
    expected_status: u8,
    expected_edges: &[(u8, u8)],
) -> Result<
    (
        Vec<MetadosisFinalizedPointV1>,
        Vec<crate::world::rpc::MetadosisWorldwideDayStatusChangeV1>,
    ),
    String,
> {
    let ports = world.validators.committee_ports();
    let all_changes = ports
        .iter()
        .map(|port| {
            world
                .rpc
                .finalized_metadosis_wwd_status_changes_on(*port, worldwide_day)
                .ok_or_else(|| {
                    format!(
                        "validator RPC {port} did not return finalized WWD {worldwide_day} status changes"
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for height in historical_lifecycle_scan_heights(minimum_height, current_height)? {
        let states = ports
            .iter()
            .map(|port| {
                world
                    .rpc
                    .metadosis_wwd_state_at(*port, worldwide_day, height)
            })
            .collect::<Vec<_>>();
        let Some(first_state) = states.first().and_then(Option::as_ref) else {
            continue;
        };
        if first_state.status != expected_status
            || !states
                .iter()
                .all(|candidate| candidate.as_ref() == Some(first_state))
        {
            continue;
        }
        let changes = all_changes
            .iter()
            .map(|validator_changes| {
                validator_changes
                    .iter()
                    .filter(|edge| edge.block_number <= height)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let first_changes = &changes[0];
        if !first_changes
            .iter()
            .map(|edge| (edge.old_status, edge.new_status))
            .eq(expected_edges.iter().copied())
            || !changes.iter().all(|candidate| candidate == first_changes)
        {
            continue;
        }
        let points = finalized_points_at_height(world, height);
        if !points
            .iter()
            .all(|point| point.block_hash == points[0].block_hash)
            || !points
                .iter()
                .all(|point| point.block_timestamp == points[0].block_timestamp)
        {
            continue;
        }
        return Ok((points, first_changes.clone()));
    }
    Err(format!(
        "no unanimous status {expected_status} with edges {expected_edges:?} in finalized heights {minimum_height}..={current_height}"
    ))
}

pub(crate) fn restart_committee_at_logical_time(
    world: &mut World,
    requested_timestamp: u64,
) -> (
    i64,
    Vec<MetadosisFinalizedPointV1>,
    u64,
    Option<crate::features::price_oracle::PendingPricePublication>,
) {
    let before_restart = finalized_points_at_common_height(world, 1);
    let before_height = before_restart[0].block_number;
    let offset = logical_time_offset(requested_timestamp, unix_time_secs());
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    let ocomp_resume = stop_ocomp_roles_before_committee_time_change(world);
    world
        .localnet
        .restart_committee_at_unix_time_offset(offset)
        .unwrap_or_else(|error| {
            panic!(
                "restart the complete committee at logical timestamp {requested_timestamp}: {error:#}"
            )
        });
    // The initial production-shaped launch starts external OCOMP roles only
    // after node RPC/TEE bootstrap. Preserve that ordering on a controlled-time
    // restart and require every validator, not only the primary, to import one
    // common finalized block before an exporter opens its projection.
    let minimum_height = before_height.saturating_add(1);
    let _ = finalized_points_at_common_height(world, minimum_height);
    let pending_price_publication =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication);
    restart_ocomp_roles_after_committee_time_change(world, ocomp_resume);
    (
        offset,
        before_restart,
        minimum_height,
        pending_price_publication,
    )
}

fn stop_ocomp_roles_before_committee_time_change(world: &mut World) -> OcompNodeFacingResumePlan {
    world
        .ocomp
        .suspend_node_facing_roles()
        .expect("suspend the exact OCOMP client inventory before node restart")
}

fn restart_ocomp_roles_after_committee_time_change(
    world: &mut World,
    plan: OcompNodeFacingResumePlan,
) {
    world
        .ocomp
        .resume_node_facing_roles(plan)
        .expect("restore the exact OCOMP client inventory after node restart");
}

fn finalized_points_at_common_height(
    world: &World,
    minimum_height: u64,
) -> Vec<MetadosisFinalizedPointV1> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ports = world.validators.committee_ports();
        let finalized = ports
            .iter()
            .map(|port| world.rpc.finalized(*port))
            .collect::<Vec<_>>();
        if finalized.iter().all(Option::is_some) {
            let common_height = finalized
                .iter()
                .flatten()
                .copied()
                .min()
                .expect("four finalized heights");
            if common_height >= minimum_height {
                let points = finalized_points_at_height(world, common_height);
                if points
                    .iter()
                    .all(|point| point.block_hash == points[0].block_hash)
                    && points
                        .iter()
                        .all(|point| point.block_timestamp == points[0].block_timestamp)
                {
                    return points;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "four validators did not converge on one finalized block at or above {minimum_height}"
        );
        sleep(Duration::from_millis(250));
    }
}

fn finalized_points_at_height(world: &World, height: u64) -> Vec<MetadosisFinalizedPointV1> {
    world
        .validators
        .committee_ports()
        .iter()
        .enumerate()
        .map(|(validator_index, port)| MetadosisFinalizedPointV1 {
            validator_index: u8::try_from(validator_index).expect("validator index fits u8"),
            block_number: height,
            block_hash: world
                .rpc
                .block_hash(*port, height)
                .and_then(|hash| B256::from_str(&hash).ok())
                .expect("canonical finalized block hash"),
            block_timestamp: world
                .rpc
                .block_timestamp(*port, height)
                .expect("canonical finalized block timestamp"),
        })
        .collect()
}

fn post_restart_convergence_target(finalized_heights: impl IntoIterator<Item = u64>) -> u64 {
    finalized_heights
        .into_iter()
        .max()
        .expect("restarted validator cohort is non-empty")
        .checked_add(1)
        .expect("post-restart convergence height does not overflow")
}

fn common_block_hash(world: &World, height: u64) -> B256 {
    let hashes = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .block_hash(port, height)
                .and_then(|hash| B256::from_str(&hash).ok())
                .expect("canonical block hash")
        })
        .collect::<Vec<_>>();
    assert!(
        hashes.iter().all(|hash| *hash == hashes[0]),
        "validators expose different canonical block {height} hashes"
    );
    hashes[0]
}

fn fresh_metadosis_wwd(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("fresh Metadosis WorldwideDay")
        .parse::<u32>()
        .expect("numeric fresh Metadosis WorldwideDay")
}

fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

fn logical_time_offset(target_timestamp: u64, now_timestamp: u64) -> i64 {
    i64::try_from(i128::from(target_timestamp) - i128::from(now_timestamp))
        .expect("testnet logical time offset fits i64")
}

#[when("one public Tribute is submitted for each scheduled OCOMP job")]
fn submit_dynamic_membership_tributes(world: &mut World) {
    let worldwide_days = world.state.ocomp_dynamic_worldwide_days.clone();
    assert_eq!(
        worldwide_days.len(),
        2,
        "dynamic-membership fixture must schedule exactly job A and job B"
    );
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 EVM key");
    let mut transaction_hashes = Vec::with_capacity(worldwide_days.len());

    for worldwide_day in &worldwide_days {
        let worldwide_day = worldwide_day.to_string();
        let transaction_hash = world
            .rpc
            .tribute_offer(&key, &worldwide_day)
            .unwrap_or_else(|| panic!("no offerTribute transaction hash for WWD {worldwide_day}"));
        assert!(
            world.rpc.wait_successful_receipt(&transaction_hash, 240),
            "Tribute for WWD {worldwide_day} did not produce a successful receipt: \
             {transaction_hash}"
        );
        transaction_hashes.push(transaction_hash);
    }

    let expected_supply = worldwide_days.len().to_string();
    let supply_deadline = Instant::now() + Duration::from_secs(60);
    while world.rpc.supply(world.validators.primary_port()).as_deref()
        != Some(expected_supply.as_str())
    {
        assert!(
            Instant::now() < supply_deadline,
            "dynamic Tributes did not produce total supply {expected_supply}"
        );
        sleep(Duration::from_millis(250));
    }

    for port in world.validators.committee_ports() {
        for worldwide_day in &worldwide_days {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                if world
                    .rpc
                    .tributes_by_day(port, *worldwide_day)
                    .is_some_and(|ids| ids.len() == 1)
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "validator on port {port} did not expose exactly one Tribute for WWD \
                     {worldwide_day}"
                );
                sleep(Duration::from_millis(250));
            }
        }
    }

    world.state.ocomp_dynamic_tribute_tx_hashes = transaction_hashes;
}

#[when("all 257 capacity owners submit one encrypted Tribute each")]
fn capacity_owners_submit_257_public_tributes(world: &mut World) {
    capacity_owners_submit_public_tributes(world, OCOMP_CAPACITY_TRIBUTE_COUNT);
}

#[when(
    expr = "{int} capacity owners submit one encrypted Tribute each at no more than two per block"
)]
fn bounded_capacity_owners_submit_public_tributes(world: &mut World, count: usize) {
    capacity_owners_submit_public_tributes(world, count);
}

fn capacity_owners_submit_public_tributes(world: &mut World, count: usize) {
    let private_keys = world.state.ocomp_capacity_tribute_private_keys.clone();
    assert!(
        private_keys.len() >= count,
        "capacity fixture retained only {} funded owners, expected at least {count}",
        private_keys.len()
    );
    let private_keys = &private_keys[..count];
    let worldwide_day = world
        .state
        .wwd
        .clone()
        .expect("capacity WorldwideDay is set");
    let mut transaction_hashes = Vec::with_capacity(private_keys.len());

    for keys in private_keys.chunks(OCOMP_CAPACITY_SUBMISSION_CONCURRENCY) {
        let batch = thread::scope(|scope| {
            keys.iter()
                .map(|private_key| {
                    let rpc = world.rpc.clone();
                    let worldwide_day = worldwide_day.clone();
                    scope.spawn(move || {
                        rpc.tribute_offer_with_params(
                            private_key,
                            &worldwide_day,
                            OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE,
                            OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO,
                            840,
                            false,
                        )
                        .ok_or_else(|| {
                            format!(
                                "capacity owner {} did not return a public Tribute tx hash",
                                rpc.address_of(private_key)
                                    .unwrap_or_else(|| "unknown".to_owned())
                            )
                        })
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "capacity Tribute submission thread panicked".to_owned())?
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .unwrap_or_else(|error| panic!("{error}"));
        for transaction_hash in &batch {
            assert!(
                world
                    .rpc
                    .wait_successful_receipt(transaction_hash, OCOMP_CAPACITY_RECEIPT_ATTEMPTS),
                "capacity Tribute transaction did not succeed: {transaction_hash}"
            );
        }
        transaction_hashes.extend(batch);
    }

    assert_eq!(
        transaction_hashes.len(),
        count,
        "not every capacity owner submitted a public Tribute"
    );
    world.state.ocomp_capacity_tribute_tx_hashes = transaction_hashes;
}

#[then(expr = "all validators observe exactly {int} public Tributes for the capacity day")]
fn all_validators_observe_public_tributes(world: &mut World, count: usize) {
    let transaction_hashes = &world.state.ocomp_capacity_tribute_tx_hashes;
    assert_eq!(transaction_hashes.len(), count);
    let expected_supply = count.to_string();
    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("capacity WorldwideDay")
        .parse::<u32>()
        .expect("numeric capacity WorldwideDay");
    for port in world.validators.committee_ports() {
        let deadline = Instant::now() + Duration::from_secs(OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS);
        loop {
            let supply_matches = world.rpc.supply(port).as_deref() == Some(&expected_supply);
            let day_matches = world
                .rpc
                .tributes_by_day(port, worldwide_day)
                .is_some_and(|ids| {
                    ids.len() == count
                        && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == count
                });
            match bounded_completion_decision(
                supply_matches && day_matches,
                Instant::now(),
                deadline,
            ) {
                BoundedCompletionDecision::Complete => break,
                BoundedCompletionDecision::Continue => sleep(Duration::from_millis(250)),
                BoundedCompletionDecision::TimedOut => {
                    panic!("validator {port} did not expose {count} distinct Tributes")
                }
            }
        }
    }
    for transaction_hash in [
        transaction_hashes
            .first()
            .expect("first capacity transaction"),
        transaction_hashes
            .last()
            .expect("last capacity transaction"),
    ] {
        world
            .projection
            .wait_for_tribute_projection(transaction_hash, 60)
            .unwrap_or_else(|error| {
                panic!(
                    "capacity boundary Tribute {transaction_hash} was not projected by every validator: {error}"
                )
            });
    }
}

#[then("three matching validator domains atomically certify the Lysis generation")]
fn validators_certify_lysis_generation(world: &mut World) {
    quorum_applies_lysis_and_creates_nod(world);
}

#[then("mineGratis is rejected while that certified generation is incomplete")]
fn mine_is_rejected_before_materialization_completion(world: &mut World) {
    let private_key = world
        .state
        .ocomp_capacity_tribute_private_keys
        .first()
        .expect("first capacity owner key")
        .clone();
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("certified generation before mining gate");
    world
        .rpc
        .assert_certified_nod_mining_blocked(
            world.validators.primary_port(),
            &private_key,
            &generation,
        )
        .expect("pre-completion certified NOD mining rejection");
}

#[then("the certified generation is materialized through at least two bounded transactions")]
fn certified_generation_crosses_multiple_materialization_batches(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("certified generation before materialization");
    let observation = world
        .rpc
        .wait_for_completed_nod_materialization(
            world.validators.primary_port(),
            &generation,
            OCOMP_CAPACITY_NOD_MATERIALIZATION_TIMEOUT_SECS,
        )
        .expect("completed multi-batch NOD materialization");
    assert!(observation.successful_batch_transactions >= 2);
    world.state.ocomp_nod_materialization = Some(observation);
}

#[then("every capacity owner enumerates one ordinary NOD with matching nodData")]
fn every_capacity_owner_has_one_materialized_nod(world: &mut World) {
    assert_materialized_capacity_owners(world, usize::MAX);
}

#[then("five deterministic capacity owners enumerate ordinary NODs with matching nodData")]
fn five_capacity_owners_have_materialized_nods(world: &mut World) {
    assert_materialized_capacity_owners(world, 5);
}

fn assert_materialized_capacity_owners(world: &mut World, limit: usize) {
    let count = world
        .state
        .ocomp_capacity_tribute_tx_hashes
        .len()
        .min(limit);
    let completion_block_number = world
        .state
        .ocomp_nod_materialization
        .as_ref()
        .expect("materialization completion before owner reads")
        .completion_block_number;
    for private_key in &world.state.ocomp_capacity_tribute_private_keys[..count] {
        let owner = world
            .rpc
            .address_of(private_key)
            .expect("capacity owner address")
            .parse()
            .expect("capacity owner address format");
        world
            .rpc
            .assert_one_materialized_nod_for_owner(
                world.validators.primary_port(),
                owner,
                completion_block_number,
            )
            .expect("ordinary owner NOD and nodData");
    }
}

#[then("mineGratis succeeds after the certified generation is completely materialized")]
fn mine_succeeds_after_materialization_completion(world: &mut World) {
    let private_key = world
        .state
        .ocomp_capacity_tribute_private_keys
        .first()
        .expect("first capacity owner key")
        .clone();
    let port = world.validators.primary_port();
    let owner = world
        .rpc
        .address_of(&private_key)
        .expect("capacity owner address")
        .parse::<alloy_primitives::Address>()
        .expect("canonical capacity owner address");
    let nod_id = world
        .rpc
        .nod_id_of_owner_by_index_on(port, owner, 0)
        .expect("capacity owner NOD read")
        .expect("capacity owner NOD is available");
    let body = world
        .rpc
        .nod_data_on(port, &nod_id)
        .expect("capacity owner NOD body");
    // Mining always burns a note, so the capacity Nod needs one deposited under
    // an asset the router registers for its reference currency — the fixture
    // genesis registers liquidity sources but no vault.
    assert_eq!(
        body.referenceCurrency, 840,
        "the settlement fixture only registers an asset for USD"
    );
    let fixture = crate::features::settlement::deploy_settlement_fixture(world);
    let cost = u128::try_from(body.costAmountMinor)
        .expect("capacity Nod cost fits a PayNote spend amount");
    let proof = crate::features::paynote::deposit_and_prove(
        world,
        port,
        &private_key,
        owner,
        fixture.asset,
        cost,
    );
    world
        .rpc
        .mine_first_materialized_capacity_nod(port, &private_key, &proof)
        .expect("post-completion mineGratis");
}

#[then("the completed materialization cursor and ordinary NOD set remain unchanged")]
fn completed_materialization_survives_restart(world: &mut World) {
    let before = world
        .state
        .ocomp_nod_materialization
        .clone()
        .expect("materialization observation before restart");
    let after = world
        .rpc
        .completed_nod_materialization(
            world.validators.primary_port(),
            world
                .state
                .ocomp_certified_generation
                .as_ref()
                .expect("certified generation after restart"),
        )
        .expect("materialization observation after restart");
    assert_eq!(after, before);
    assert_materialized_capacity_owners(world, 5);
}

#[when("the committee logical clock reaches the public capacity processing time")]
fn committee_clock_reaches_public_capacity_processing(world: &mut World) {
    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("capacity WorldwideDay")
        .parse::<u32>()
        .expect("numeric capacity WorldwideDay");
    let finalized_points = finalized_points_at_common_height(world, 1);
    let common_height = finalized_points[0].block_number;
    let states = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .metadosis_wwd_state_at(port, worldwide_day, common_height)
        })
        .collect::<Vec<_>>();
    let state = states[0]
        .clone()
        .expect("capacity WorldwideDay exists at the common finalized height");
    assert!(
        states
            .iter()
            .all(|candidate| candidate.as_ref() == Some(&state)),
        "validators expose different capacity WorldwideDay state before the controlled-time transition"
    );
    assert_eq!(
        state.status, 2,
        "capacity WorldwideDay must remain in OFFERING until all 257 receipts and projections are observed"
    );

    let target = first_protocol_cycle_at_or_after(world, state.scheduled_process_time);
    let _ = restart_committee_at_logical_time(world, target);
}

#[then("Metadosis creates one finalized JobIntent from that public Tribute")]
fn metadosis_creates_finalized_job_intent(world: &mut World) {
    let expected_wwd = world
        .state
        .wwd
        .as_deref()
        .expect("measurement WorldwideDay")
        .parse::<u32>()
        .expect("numeric measurement WorldwideDay");
    let activation_height = world
        .state
        .ocomp_activation_height
        .expect("prepared OCOMP activation height");
    let primary = world.validators.primary_port();
    let schedule = world
        .rpc
        .metadosis_wwd_state_on(primary, expected_wwd)
        .expect("read authoritative Metadosis schedule before waiting for JobIntent");
    let mut previous_timestamp = world
        .rpc
        .latest_block_timestamp(primary)
        .expect("read canonical timestamp before waiting for JobIntent");
    let mut progress_deadline =
        Instant::now() + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
    loop {
        let now = Instant::now();
        let canonical_timestamp = world
            .rpc
            .latest_block_timestamp(primary)
            .expect("read canonical timestamp while waiting for Metadosis schedule");
        match monotonic_progress_decision(
            canonical_timestamp,
            schedule.scheduled_process_time,
            previous_timestamp,
            now,
            progress_deadline,
        ) {
            ProgressWaitDecision::Reached => break,
            ProgressWaitDecision::Progressed => {
                previous_timestamp = canonical_timestamp;
                progress_deadline = now + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
            }
            ProgressWaitDecision::Waiting => {}
            ProgressWaitDecision::Stalled => {
                panic!(
                    "canonical time stalled before the Metadosis JobIntent schedule: \
                     worldwide_day={expected_wwd}, status={}, canonical_timestamp={canonical_timestamp}, \
                     offering_end={}, scheduled_process_time={}, finalized={:?}",
                    schedule.status,
                    schedule.offering_end,
                    schedule.scheduled_process_time,
                    world
                        .validators
                        .committee_ports()
                        .into_iter()
                        .map(|port| world.rpc.finalized(port))
                        .collect::<Vec<_>>()
                );
            }
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive while canonical time reaches the Metadosis schedule");
        sleep(Duration::from_millis(500));
    }

    let deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let request = loop {
        let observed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_on(port, activation_height)
                    .unwrap_or_else(|error| {
                        panic!("observe OCOMP request on port {port}: {error:#}")
                    })
            })
            .collect::<Vec<_>>();
        if observed.iter().all(Option::is_some) {
            let first = observed[0].clone().expect("all requests are present");
            assert!(
                observed
                    .iter()
                    .all(|request| request.as_ref() == Some(&first)),
                "validators expose different finalized OCOMP JobIntent requests"
            );
            break first;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive while Metadosis reaches the request transition");
        assert!(
            Instant::now() < deadline,
            "Metadosis did not create a finalized public JobIntent in bounded measurement time"
        );
        sleep(Duration::from_millis(500));
    };
    assert_eq!(request.worldwide_day, expected_wwd);
    assert_ne!(request.intent_id, B256::ZERO);
    assert_ne!(request.activation_preconditions_hash, B256::ZERO);
    assert_eq!(
        request.open_height,
        request
            .finality_recorded_height
            .checked_add(4)
            .expect("public finality height admits the fixed depth"),
        "the public voting window must open exactly four blocks after recorded finality"
    );
    assert!(
        request.deadline_height > request.open_height,
        "JobIntent deadline is not exclusive and after its open height"
    );
    world.state.ocomp_job_request = Some(request);
}

#[when("an OCOMP successor is preloaded and activated while that V1 job remains pending")]
fn activate_ocomp_successor_with_pending_v1_job(world: &mut World) {
    let limits = poc_schema_limits();
    let install = world
        .ocomp
        .canonical_fork_install()
        .expect("read canonical OCOMP genesis authority");
    let initial_authority = registry_authority_from_install(&install);
    let initial_bundle_hash = initial_authority.request_profile.protocol_bundle_hash;
    let pending_request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("pending V1 JobIntent before OCOMP successor activation");
    assert_pending_v1_at_common_finality(world, initial_bundle_hash, None);

    let mut successor_bundle = install.protocol_bundle.clone();
    successor_bundle.protocol_version = successor_bundle
        .protocol_version
        .checked_add(1)
        .expect("OCOMP protocol version increment");
    successor_bundle.fork_id = B256::repeat_byte(0xa1);
    successor_bundle.request_semantics_version = successor_bundle
        .request_semantics_version
        .checked_add(1)
        .expect("OCOMP request-semantics version increment");
    successor_bundle.lysis_program_semantics_hash = B256::repeat_byte(0xa2);
    let successor_bundle_hash = successor_bundle
        .protocol_bundle_hash(&limits)
        .expect("hash canonical OCOMP successor bundle");
    let successor_identity = world
        .ocomp
        .stage_successor_bundle(&successor_bundle)
        .expect("publish OCOMP successor bundle into every domain catalog");
    assert_eq!(
        successor_identity.protocol_bundle_hash, successor_bundle_hash,
        "published successor identity differs from the canonical bundle"
    );

    // This is the only process restart in the upgrade path. It happens before
    // the proposal so every Node and SnapshotExporter has both V1 and V2 loaded.
    // The activation-height assertions below prove that no process is restarted
    // when Update promotes V2.
    let before_restart = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("finality before successor preload restart");
    let ocomp_resume = stop_ocomp_roles_before_committee_time_change(world);
    world
        .localnet
        .restart_committee_preserving_enclaves()
        .expect("restart committee once to preload the successor bundle");
    let full_node = world.validators.joiner_index();
    let full_node_wire = u8::try_from(full_node).expect("FullNode index fits u8");
    let (old_full_node_pid, old_status) = world
        .localnet
        .owned_full_node_process(full_node)
        .expect("capture the owned FullNode before test-only V2 preload");
    assert!(old_status.is_none(), "FullNode exited before preload");
    world
        .ocomp
        .stop_keyless_full_node_roles(full_node_wire)
        .expect("stop only this FullNode's clients before its preserved preload restart");
    let (stopped_status, new_full_node_pid) = world
        .localnet
        .restart_keyless_full_node_preserving_enclave(full_node, old_full_node_pid, 0)
        .expect("preload both public bundles while preserving FullNode identity and data");
    assert_ne!(old_full_node_pid, new_full_node_pid);
    eprintln!("OCOMP_FULLNODE_PRELOAD old_pid={old_full_node_pid} new_pid={new_full_node_pid} exit={stopped_status}");
    world
        .ocomp
        .start_keyless_full_node_roles(full_node_wire)
        .expect("restore the FullNode's exporter and V1 worker before proposal");
    for validator_index in 0..world.validators.size() {
        let port = world.validators.http_port(validator_index);
        assert!(
            world.rpc.wait_block(port, before_restart, 60).is_some(),
            "validator-{validator_index} did not restore finality after successor preload"
        );
    }
    let convergence_target = post_restart_convergence_target(
        world.validators.committee_ports().into_iter().map(|port| {
            world
                .rpc
                .finalized(port)
                .expect("finalized height after successor preload restart")
        }),
    );
    let _ = finalized_points_at_common_height(world, convergence_target);
    restart_ocomp_roles_after_committee_time_change(world, ocomp_resume);
    world
        .ocomp
        .activate_successor_workers(successor_identity)
        .expect("start one V2 Worker lane in every validator domain");
    world
        .ocomp
        .ensure_successor_workers_ready()
        .expect("all V2 Worker lanes register before governance activation");
    world
        .ocomp
        .ensure_keyless_full_node_roles_ready(full_node_wire)
        .expect("both keyless compute lanes are registered before governance activation");
    let mut pre_proposal_ports = world.validators.committee_ports();
    pre_proposal_ports.push(world.validators.http_port(full_node));
    world
        .rpc
        .wait_finalized_checkpoint(&pre_proposal_ports, before_restart, 120)
        .expect("all five nodes recover exact pre-restart finality before the proposal");
    let (pre_proposal_pid, pre_proposal_status) = world
        .localnet
        .owned_full_node_process(full_node)
        .expect("recheck FullNode incarnation after finalized preload recovery");
    assert_eq!(pre_proposal_pid, new_full_node_pid);
    assert!(pre_proposal_status.is_none());

    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("head before OCOMP successor proposal");
    let activation_height = head
        .checked_add(world.state.voting_window)
        .and_then(|height| height.checked_add(30))
        .expect("OCOMP successor activation height");
    assert!(
        activation_height < pending_request.deadline_height,
        "test-only V2 activation must occur before the pending V1 deadline"
    );
    let successor = OcompSuccessorV1 {
        activation_height,
        predecessor_protocol_bundle_hash: initial_bundle_hash,
        authority: OcompProtocolAuthorityV1 {
            request_profile: OcompRequestProfile {
                fork_id: successor_bundle.fork_id,
                protocol_bundle_hash: successor_bundle_hash,
                correctness_profile_id: successor_bundle.correctness_profile_id,
                ..initial_authority.request_profile.clone()
            },
            protocol_bundle: successor_bundle,
        },
    };
    successor
        .validate_against(&initial_authority, head, &limits)
        .expect("successor obeys the immutable predecessor policy");
    let active_version = world
        .rpc
        .active_version()
        .expect("read active protocol version");
    let proposed_version = active_version
        .checked_add(1)
        .expect("protocol version increment");
    let successor_hex = hex::encode(
        successor
            .encode_canonical(&limits)
            .expect("encode canonical OCOMP successor"),
    );
    let payload = serde_json::json!({
        "version": format!(
            "{}.{}",
            proposed_version >> 24,
            proposed_version & 0x00ff_ffff
        ),
        "activationHeight": activation_height,
        "info": "OCOMP successor activation with pending V1 lineage",
        "ocompSuccessor": successor_hex,
    })
    .to_string();

    let mut node_pids_before_activation = (0..world.validators.size())
        .map(|validator_index| {
            world
                .localnet
                .validator_pid(validator_index)
                .unwrap_or_else(|error| {
                    panic!("capture validator-{validator_index} pid before activation: {error}")
                })
        })
        .collect::<Vec<_>>();
    node_pids_before_activation.push(new_full_node_pid);
    let proposer = world
        .validators
        .operator("validator-0")
        .expect("resolve OCOMP successor proposer");
    let proposal_id = world.state.proposal_id;
    let propose_tx = world
        .rpc
        .send_propose(&proposer, &format!("{UPDATE_ADDR:#x}"), &payload)
        .expect("submit OCOMP successor proposal");
    assert!(
        world.rpc.wait_successful_receipt(&propose_tx, 40),
        "OCOMP successor proposal transaction failed: {propose_tx}"
    );
    for validator_index in 0..3 {
        let validator = world.validators.get(validator_index);
        let vote_tx = world
            .rpc
            .cast_vote(&validator, proposal_id, true)
            .unwrap_or_else(|error| {
                panic!("validator-{validator_index} OCOMP successor vote failed: {error}")
            });
        assert!(
            world.rpc.wait_successful_receipt(&vote_tx, 40),
            "validator-{validator_index} OCOMP successor vote receipt failed: {vote_tx}"
        );
    }
    assert_eq!(
        world.rpc.wait_active_version(proposed_version, 60),
        Some(proposed_version),
        "Update did not activate the OCOMP successor at the scheduled height"
    );
    for (validator_index, port) in world.validators.committee_ports().into_iter().enumerate() {
        assert!(
            world.rpc.wait_block(port, activation_height, 60).is_some(),
            "validator-{validator_index} did not finalize the OCOMP activation height"
        );
        assert_eq!(
            world.rpc.active_ocomp_protocol_bundle_hash_on(port),
            Some(successor_bundle_hash),
            "validator-{validator_index} did not expose V2 as the active OCOMP bundle"
        );
    }
    let mut node_pids_after_activation = (0..world.validators.size())
        .map(|validator_index| {
            world
                .localnet
                .validator_pid(validator_index)
                .unwrap_or_else(|error| {
                    panic!("capture validator-{validator_index} pid after activation: {error}")
                })
        })
        .collect::<Vec<_>>();
    let mut all_ports = world.validators.committee_ports();
    all_ports.push(world.validators.http_port(full_node));
    world
        .rpc
        .wait_finalized_checkpoint(&all_ports, activation_height, 120)
        .expect("committee and keyless FullNode share the exact finalized test-only V2 activation");
    let (pid, status) = world
        .localnet
        .owned_full_node_process(full_node)
        .expect("observe the same owned FullNode at activation");
    assert!(status.is_none());
    node_pids_after_activation.push(pid);
    assert_eq!(
        node_pids_after_activation, node_pids_before_activation,
        "a validator or keyless FullNode restarted at test-only V2 activation"
    );
    assert_pending_v1_at_common_finality(
        world,
        initial_bundle_hash,
        Some((activation_height, successor_bundle_hash)),
    );
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all V1 and V2 OCOMP runtime roles stay alive after activation");

    world.state.proposed_version = Some(proposed_version);
    world.state.ocomp_successor_bundle_hash = Some(successor_bundle_hash);
    world.state.ocomp_successor_activation_height = Some(activation_height);
    world.state.ocomp_successor_node_pids_before_activation = node_pids_before_activation;
    world.state.ocomp_successor_node_pids_after_activation = node_pids_after_activation;
}

#[then("activation keeps every validator process alive and the pending job pinned to V1")]
fn activation_preserves_nodes_and_pending_v1_pin(world: &mut World) {
    let install = world
        .ocomp
        .canonical_fork_install()
        .expect("read canonical OCOMP genesis authority after successor activation");
    let initial_bundle_hash = install.request_profile.protocol_bundle_hash;
    let successor_bundle_hash = world
        .state
        .ocomp_successor_bundle_hash
        .expect("successor bundle hash evidence");
    assert_ne!(successor_bundle_hash, initial_bundle_hash);
    assert_eq!(
        world.state.ocomp_successor_node_pids_after_activation,
        world.state.ocomp_successor_node_pids_before_activation,
        "validator process identities changed at OCOMP activation"
    );
    assert_pending_v1_at_common_finality(
        world,
        initial_bundle_hash,
        Some((
            world
                .state
                .ocomp_successor_activation_height
                .expect("activation height"),
            successor_bundle_hash,
        )),
    );
    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.active_ocomp_protocol_bundle_hash_on(port),
            Some(successor_bundle_hash),
            "one validator does not expose V2 while the pending JobIntent remains on V1"
        );
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP roles remain live after successor activation");
}

fn assert_pending_v1_at_common_finality(
    world: &mut World,
    expected_bundle: B256,
    activation: Option<(u64, B256)>,
) {
    assert!(
        world.state.ocomp_pending_v1_workers_held,
        "pending-V1 proof requires the deliberately held worker cohort"
    );
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("canonical V1 request");
    let target = world
        .rpc
        .finalized_result(world.validators.primary_port())
        .expect("read pending-V1 finalized anchor")
        .max(activation.map_or(request.finality_recorded_height, |(height, _)| height));
    let checkpoint = wait_for_common_finalized_checkpoint(world, target, "pending V1 activation");
    assert!(
        checkpoint.height < request.deadline_height,
        "V1 expired before activation proof"
    );
    for port in world.validators.committee_ports() {
        let record = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, checkpoint.height)
            .expect("read V1 at the common finalized activation checkpoint");
        assert_eq!(
            record.status,
            if checkpoint.height < request.open_height {
                OcompJobStatus::AwaitingFinality
            } else {
                OcompJobStatus::VotingOpen
            },
            "V1 is not in its height-appropriate pending state"
        );
        if let Some((_, bundle)) = activation {
            assert_eq!(
                world
                    .rpc
                    .active_ocomp_protocol_bundle_hash_at_on(port, checkpoint.height)
                    .expect("read active bundle at the exact pending-V1 activation checkpoint"),
                bundle
            );
        }
        assert!(
            record.terminal.is_none(),
            "pending V1 already has a terminal outcome"
        );
        assert_eq!(record.intent.protocol_bundle_hash, expected_bundle);
        let finalized = record
            .finalized
            .as_ref()
            .expect("pending job has canonical finality");
        assert_eq!(finalized.job_id, request.job_id);
        assert_eq!(finalized.open_height, request.open_height);
        assert_eq!(finalized.deadline_height, request.deadline_height);
        assert!(
            finalized.quorum.is_none(),
            "pending V1 already has a result quorum"
        );
    }
}

fn registry_authority_from_install(
    install: &outbe_metadosis::OcompForkInstallV1,
) -> OcompProtocolAuthorityV1 {
    OcompProtocolAuthorityV1 {
        request_profile: OcompRequestProfile {
            chain_id: install.request_profile.chain_id,
            genesis_hash: install.request_profile.genesis_hash,
            fork_id: install.request_profile.fork_id,
            protocol_bundle_hash: install.request_profile.protocol_bundle_hash,
            correctness_profile_id: install.request_profile.correctness_profile_id,
            capacity_profile: install.request_profile.capacity_profile.clone(),
            source_availability_policy_id: install.request_profile.source_availability_policy_id,
        },
        protocol_bundle: install.protocol_bundle.clone(),
    }
}

fn assert_job_pinned_on_every_validator(world: &World, intent_id: B256, expected: B256) {
    for (validator_index, port) in world.validators.committee_ports().into_iter().enumerate() {
        let record = world
            .rpc
            .finalized_ocomp_job_record_on(port, intent_id)
            .unwrap_or_else(|| {
                panic!("validator-{validator_index} cannot read finalized OCOMP JobIntent")
            });
        assert_eq!(
            record.intent.protocol_bundle_hash, expected,
            "validator-{validator_index} changed the pending JobIntent bundle pin"
        );
    }
}

#[when("a fresh post-activation Tribute completes through the V2 worker lane")]
fn fresh_post_activation_tribute_completes_on_v2(world: &mut World) {
    let initial_bundle_hash = world
        .ocomp
        .canonical_fork_install()
        .expect("read V1 authority before retirement")
        .request_profile
        .protocol_bundle_hash;
    let successor_bundle_hash = world
        .state
        .ocomp_successor_bundle_hash
        .expect("V2 activation evidence");
    let ports = world.validators.committee_ports();
    let completed_height = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .and_then(|accountability| accountability.quorum_height)
        .expect("canonical V1 quorum height");
    let completion = wait_for_common_finalized_checkpoint(world, completed_height, "V1 release");
    let retention = ports
        .iter()
        .copied()
        .map(|port| {
            let (retiring, live, until) = world
                .rpc
                .ocomp_retention_state_at_on(port, initial_bundle_hash, completion.height)
                .expect("read V1 retention at its exact canonical completion checkpoint");
            assert_eq!(
                retiring, initial_bundle_hash,
                "V1 must remain retiring immediately after its final lineage releases"
            );
            assert_eq!(
                live, 0,
                "completed V1 JobIntent must release its Registry lineage"
            );
            until
        })
        .collect::<Vec<_>>();
    assert!(retention.iter().all(|height| *height == retention[0]));
    assert!(
        retention[0] > completion.height,
        "V1 retired before its deterministic retention interval elapsed"
    );
    world.state.ocomp_predecessor_retention_until = Some(retention[0]);

    let first_wwd = WorldwideDay::new(fresh_metadosis_wwd(world));
    let successor_wwd = WorldwideDay::from_timestamp(
        first_wwd
            .start_timestamp()
            .checked_add(86_400)
            .expect("next WorldwideDay timestamp"),
    );
    let successor_wwd_value = successor_wwd.value();
    let primary = world.validators.primary_port();
    let schedule = world
        .rpc
        .metadosis_wwd_state_on(primary, successor_wwd_value)
        .expect("next WorldwideDay exists before its OFFERING window");
    assert!(
        schedule.status <= 1,
        "fresh successor WorldwideDay passed OFFERING before V2 Tribute submission"
    );

    let offering_target = schedule
        .lookback_end
        .checked_add(1)
        .expect("successor OFFERING timestamp");
    let _ = restart_committee_at_logical_time(world, offering_target);
    let offering_deadline = Instant::now() + RATCHET_STALL_TIMEOUT;
    loop {
        let states = ports
            .iter()
            .copied()
            .map(|port| world.rpc.metadosis_wwd_state_on(port, successor_wwd_value))
            .collect::<Vec<_>>();
        if states
            .iter()
            .all(|state| state.as_ref().is_some_and(|state| state.status == 2))
        {
            break;
        }
        assert!(
            Instant::now() < offering_deadline,
            "successor WorldwideDay did not reach OFFERING: {states:?}"
        );
        sleep(Duration::from_millis(250));
    }
    world
        .ocomp
        .ensure_successor_workers_ready()
        .expect("V2 workers survive the post-activation restart overlap");

    // Hold before submitting the V2 Tribute: it may trigger the job before
    // the explicit processing-time jump. The restart inventory keeps it held.
    for validator_index in 0..4_u8 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 1,
            })
            .expect("hold V2 workers before the new Tribute");
    }

    let offerer = world
        .validators
        .by_name("validator-1")
        .expect("validator-1 V2 Tribute owner")
        .evm_key()
        .expect("validator-1 V2 Tribute key");
    let tribute_tx = world
        .rpc
        .tribute_offer(&offerer, &successor_wwd_value.to_string())
        .expect("submit fresh V2-era Tribute");
    assert!(
        world.rpc.wait_successful_receipt(&tribute_tx, 240),
        "V2-era Tribute transaction failed: {tribute_tx}"
    );
    world
        .projection
        .wait_for_tribute_projection(&tribute_tx, 240)
        .expect("all exporters project the V2-era Tribute");

    let processing_target =
        first_protocol_cycle_at_or_after(world, schedule.scheduled_process_time);
    let _ = restart_committee_at_logical_time(world, processing_target);
    // Request discovery and the held-worker barrier consume the same existing
    // V2 artifact budget; do not reset it when the cohort is released.
    let completion_deadline =
        Instant::now() + Duration::from_secs(OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS);

    let from_height = world
        .state
        .ocomp_successor_activation_height
        .expect("V2 activation height");
    let request_deadline = completion_deadline;
    let request = loop {
        let observed = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_for_worldwide_day_on(
                        port,
                        from_height,
                        successor_wwd_value,
                    )
                    .unwrap_or_else(|error| panic!("observe V2 request on port {port}: {error:#}"))
            })
            .collect::<Vec<_>>();
        if observed.iter().all(Option::is_some) {
            let first = observed[0].clone().expect("all V2 requests are present");
            assert!(observed
                .iter()
                .all(|candidate| candidate.as_ref() == Some(&first)));
            break first;
        }
        assert!(
            Instant::now() < request_deadline,
            "fresh V2 JobIntent was not finalized for WWD {successor_wwd_value}"
        );
        sleep(Duration::from_millis(500));
    };
    assert_eq!(request.worldwide_day, successor_wwd_value);
    assert_job_pinned_on_every_validator(world, request.intent_id, successor_bundle_hash);

    wait_case_one_worker_release(world, &request, successor_bundle_hash, completion_deadline)
        .expect("all four current V2 node incarnations dispatched the exact exported job");
    arm_case_one_artifact_phase(
        world,
        successor_bundle_hash,
        completion_deadline
            .checked_duration_since(Instant::now())
            .expect("V2 artifact budget expired before worker release"),
    )
    .expect("arm exact V2 node incarnations before releasing workers");
    world
        .ocomp
        .restart_worker_cohort(&[(0, 1), (1, 1), (2, 1), (3, 1)])
        .expect("release all four V2 workers before the shared readiness wait");
    world
        .ocomp
        .ensure_successor_workers_ready()
        .expect("V2 workers reconnect after the held processing-time restart");
    let completed = loop {
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::Completed)
        }) {
            let first = records[0].clone().expect("all V2 records are present");
            assert!(records
                .iter()
                .all(|candidate| candidate.as_ref() == Some(&first)));
            break first;
        }
        world
            .ocomp
            .ensure_successor_workers_ready()
            .expect("V2 workers stay live until the fresh job completes");
        assert!(
            Instant::now() < completion_deadline,
            "fresh V2 JobIntent did not complete"
        );
        sleep(Duration::from_millis(500));
    };
    assert!(
        completed.finalized.as_ref().is_some(),
        "completed V2 record lacks finality"
    );
    verify_case_one_completed_artifacts(world, request.intent_id, successor_bundle_hash)
        .expect("V2 job has four exact computations and canonical-voter submission evidence");
    let binding = completed
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.completed_binding.as_ref())
        .expect("test-only V2 has an exact canonical completed binding");
    verify_successor_full_node_result(world, binding.job_id, binding.result_digest);
    world.state.ocomp_successor_job_request = Some(request);
}

fn verify_successor_full_node_result(world: &mut World, job_id: B256, expected_digest: B256) {
    let index = world.validators.joiner_index();
    let expected_pid = *world
        .state
        .ocomp_successor_node_pids_after_activation
        .last()
        .expect("FullNode post-preload incarnation");
    let path = full_node_local_result_path(world, job_id);
    let deadline = Instant::now() + Duration::from_secs(OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS);
    loop {
        let (pid, status) = world
            .localnet
            .owned_full_node_process(index)
            .expect("observe exact FullNode during test-only V2 result verification");
        assert_eq!(pid, expected_pid);
        assert!(
            status.is_none(),
            "FullNode exited before verifying test-only V2"
        );
        world
            .ocomp
            .ensure_keyless_full_node_roles_alive(u8::try_from(index).unwrap())
            .expect("FullNode compute clients remain alive");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let result = LysisResultV1::decode_canonical(&bytes, &poc_schema_limits())
                    .expect("decode FullNode's independently computed test-only V2 result");
                assert_eq!(result.job_id, job_id);
                assert_eq!(
                    result.result_digest(&poc_schema_limits()).unwrap(),
                    expected_digest
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot read FullNode result: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "FullNode did not verify the exact test-only V2 result"
        );
        sleep(Duration::from_millis(250));
    }
    let mut ports = world.validators.committee_ports();
    ports.push(world.validators.http_port(index));
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("post-result target advances beyond every expected live node");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 120)
        .expect(
            "all five nodes continue at the same exact finalized checkpoint after V2 verification",
        );
    assert!(!dynamic_vote_submission_path(world, index, job_id)
        .try_exists()
        .expect("observe FullNode's absence of vote submissions"));
    let (pid, status) = world.localnet.owned_full_node_process(index).unwrap();
    assert_eq!(pid, expected_pid);
    assert!(status.is_none());
}

#[then("the released V1 authority retires after its retention deadline")]
fn released_v1_authority_retires_after_retention(world: &mut World) {
    let initial_bundle_hash = world
        .ocomp
        .canonical_fork_install()
        .expect("read V1 authority for retirement assertion")
        .request_profile
        .protocol_bundle_hash;
    let successor_bundle_hash = world
        .state
        .ocomp_successor_bundle_hash
        .expect("V2 authority for retirement assertion");
    let retention_until = world
        .state
        .ocomp_predecessor_retention_until
        .expect("captured V1 retention deadline");
    let target = retention_until.saturating_add(1);
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, target, 120),
            "validator on port {port} did not reach V1 retirement height {target}"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let retired_everywhere = world.validators.committee_ports().into_iter().all(|port| {
            world.rpc.retiring_ocomp_protocol_bundle_hash_on(port) == Some(B256::ZERO)
                && world
                    .rpc
                    .ocomp_live_lineage_count_on(port, initial_bundle_hash)
                    == Some(0)
                && world
                    .rpc
                    .ocomp_retention_until_on(port, initial_bundle_hash)
                    == Some(0)
                && world.rpc.active_ocomp_protocol_bundle_hash_on(port)
                    == Some(successor_bundle_hash)
        });
        if retired_everywhere {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "V1 authority did not retire everywhere"
        );
        sleep(Duration::from_millis(250));
    }
    assert!(
        world.state.ocomp_successor_job_request.is_some(),
        "retirement evidence requires the fresh V2 job"
    );
}

#[when("the production OCOMP domains process that finalized JobIntent")]
fn production_ocomp_domains_process_job_intent(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized public JobIntent");
    let generation_exists = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        generation_exists
            .iter()
            .all(|exists| *exists == Some(false)),
        "Nod generation already exists or cannot be read at the finalized request block: \
         {generation_exists:?}"
    );
    let primary = world.validators.primary_port();
    // ResultVoteV1 carriers are signed by the role-scoped OCOMP delegates,
    // not by the validator owner EOAs.  Probe those exact sender accounts so
    // unrelated owner-side protocol credits cannot masquerade as carrier fees.
    world.state.ocomp_validator_balances_before = (0..world.validators.size())
        .map(|validator_index| {
            let validator_index = u8::try_from(validator_index)
                .expect("OCOMP validator index fits the wire representation");
            let address = world
                .ocomp
                .ocomp_delegate_address(validator_index)
                .expect("derive OCOMP delegate address");
            let balance = world
                .rpc
                .balance_on(primary, &format!("{address:#x}"))
                .expect("read OCOMP delegate balance before result votes");
            (address, balance)
        })
        .collect();
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("production OCOMP domains remain alive while processing the JobIntent");
    if world.state.ocomp_pending_v1_workers_held {
        release_pending_v1_workers_after_exact_exports(world);
    }
}

fn release_pending_v1_workers_after_exact_exports(world: &mut World) {
    let timeout = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("held V1 request");
    let install = world.ocomp.canonical_fork_install().expect("V1 authority");
    let activation = match (
        world.state.ocomp_successor_activation_height,
        world.state.ocomp_successor_bundle_hash,
    ) {
        (Some(height), Some(bundle)) => Some((height.max(request.open_height), bundle)),
        (None, None) => None,
        _ => panic!("incomplete test-only V2 activation evidence before worker release"),
    };
    assert_pending_v1_at_common_finality(
        world,
        install.request_profile.protocol_bundle_hash,
        activation,
    );
    wait_case_one_worker_release(
        world,
        &request,
        install.request_profile.protocol_bundle_hash,
        timeout,
    )
    .expect("all four current V1 node incarnations dispatched the exact exported job");
    arm_case_one_artifact_phase(
        world,
        install.request_profile.protocol_bundle_hash,
        Duration::from_secs(600),
    )
    .expect("arm exact V1 incarnations before releasing workers");
    world
        .ocomp
        .restart_worker_cohort(&[(0, 0), (1, 0), (2, 0), (3, 0)])
        .expect("release all held V1 workers before waiting for individual readiness");
    world.state.ocomp_pending_v1_workers_held = false;
}

fn case_one_compute_started_line(text: &str, job_id: B256) -> Option<&str> {
    let expected = format!("embedded OCOMP computation started job_id={job_id:#x}");
    text.lines().find(|line| {
        line.split_once(" INFO ")
            .and_then(|(_, message)| message.split_once("outbe_chain::ocomp_exex: "))
            .is_some_and(|(_, message)| message.trim_end() == expected)
    })
}

/// Keep workers held until every current node has consumed its exact export
/// ACK and dispatched computation. Completed is not a substitute: a validator
/// first observing Completed need not start its own computation.
fn wait_case_one_worker_release(
    world: &mut World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
    bundle_hash: B256,
    deadline: Instant,
) -> eyre::Result<()> {
    let ports = world.validators.committee_ports();
    ensure!(
        ports.len() == 4 && world.validators.size() == 4,
        "worker release requires all four founders"
    );
    ensure!(Instant::now() < deadline, "worker release budget expired");
    world.localnet.ensure_committee_alive()?;
    let pids = (0..4)
        .map(|index| {
            let (pid, status) = world.localnet.owned_validator_process(index)?;
            ensure!(
                status.is_none(),
                "validator-{index} exited before worker release"
            );
            // Validate all launch captures before entering any polling wait. The
            // capture predates spawn, so an already-emitted marker is not lost.
            world.localnet.node_launch_log(index, pid)?;
            Ok(pid)
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    let limits = poc_schema_limits();
    let checkpoint = world.rpc.checkpoint_at(ports[0], request.request_height)?;
    ensure!(
        checkpoint.block_hash == request.request_block_hash,
        "worker release request checkpoint changed"
    );
    let mut bundles = Vec::new();
    for (index, &port) in ports.iter().enumerate() {
        ensure!(
            world.rpc.checkpoint_at(port, request.request_height)? == checkpoint,
            "validator-{index} disagrees on the request checkpoint"
        );
        let path = world
            .ocomp
            .domain_root(u8::try_from(index)?)?
            .join("protocol-bundles-v1")
            .join(format!("{}.ocb1", hex::encode(bundle_hash.as_slice())));
        let bundle = outbe_ocomp_protocol::profile::ProtocolBundleV1::decode_canonical(
            &std::fs::read(path)?,
            &limits,
        )?;
        ensure!(
            bundle.protocol_bundle_hash(&limits)? == bundle_hash,
            "validator-{index} has the wrong worker release bundle"
        );
        bundles.push(bundle);
    }
    loop {
        ensure!(
            Instant::now() < deadline,
            "four exported compute dispatches exceeded the release budget"
        );
        world.ocomp.ensure_validator_roles_alive()?;
        for (index, &expected_pid) in pids.iter().enumerate() {
            let (pid, status) = world.localnet.owned_validator_process(index)?;
            ensure!(
                pid == expected_pid && status.is_none(),
                "validator-{index} incarnation changed or exited during worker release"
            );
        }
        let heights = ports
            .iter()
            .map(|&port| world.rpc.finalized_result(port))
            .collect::<eyre::Result<Vec<_>>>()?;
        ensure!(
            heights
                .iter()
                .all(|height| *height < request.deadline_height),
            "job deadline reached before all four workers could be released"
        );
        let height = *heights.iter().min().unwrap();
        let common = world.rpc.checkpoint_at(ports[0], height)?;
        for &port in &ports[1..] {
            ensure!(
                world.rpc.checkpoint_at(port, height)? == common,
                "worker release peers disagree on exact finalized hash/root"
            );
        }
        if height >= request.open_height {
            let mut observations = Vec::new();
            let mut expected_record = None;
            for (index, &port) in ports.iter().enumerate() {
                let record = world
                    .rpc
                    .ocomp_job_record_at_on(port, request.intent_id, height)?;
                ensure!(
                    record.status == OcompJobStatus::VotingOpen
                        && record.intent.protocol_bundle_hash == bundle_hash,
                    "held worker job changed bundle or is no longer VotingOpen"
                );
                let finalized = record
                    .finalized
                    .as_ref()
                    .ok_or_else(|| eyre!("held job lacks finality"))?;
                ensure!(
                    finalized.job_id == request.job_id
                        && finalized.open_height == request.open_height
                        && finalized.deadline_height == request.deadline_height
                        && finalized.finality_recorded_height == request.finality_recorded_height,
                    "held worker job changed its canonical identity or window"
                );
                if let Some(expected) = &expected_record {
                    ensure!(
                        &record == expected,
                        "held worker job differs across finalized peers"
                    );
                }
                let export = crate::internal::ocomp_worker_outage::observe_export(
                    world.ocomp.domain_root(u8::try_from(index)?)?,
                    u8::try_from(index)?,
                    &record,
                    checkpoint,
                    &bundles[index],
                )?;
                expected_record = Some(record);
                let text = world.localnet.node_launch_log(index, pids[index])?;
                if let (Some(export), Some(marker)) =
                    (export, case_one_compute_started_line(&text, request.job_id))
                {
                    observations.push(serde_json::json!({
                        "validator_index": index, "node_pid": pids[index],
                        "export": export, "compute_started": marker,
                    }));
                }
            }
            if observations.len() == 4 {
                // Recheck after filesystem/RPC observation, not merely before
                // it: no stale live identity or expired window authorizes work.
                for (index, &port) in ports.iter().enumerate() {
                    let (pid, status) = world.localnet.owned_validator_process(index)?;
                    ensure!(
                        pid == pids[index] && status.is_none(),
                        "worker release lost its live node owner"
                    );
                    ensure!(
                        world.rpc.finalized_result(port)? < request.deadline_height,
                        "job deadline crossed during worker release observation"
                    );
                }
                ensure!(
                    Instant::now() < deadline,
                    "worker release budget expired during observation"
                );
                eprintln!(
                    "OCOMP_WORKER_RELEASE_V1 {}",
                    serde_json::json!({
                        "checkpoint": {
                            "height": common.height,
                            "block_hash": common.block_hash,
                            "state_root": common.state_root,
                        }, "job_id": request.job_id,
                        "bundle_hash": bundle_hash, "observations": observations,
                    })
                );
                return Ok(());
            }
        }
        sleep(Duration::from_millis(100));
    }
}

#[when("validator 2 prepares one valid vote without broadcasting it")]
fn validator_two_prepares_held_vote(world: &mut World) {
    const VALIDATOR_INDEX: usize = 2;

    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let vote_bytes = loop {
        let finalized_height = world
            .rpc
            .finalized_result(primary)
            .expect("observe finalized height while waiting for public result vote");
        let public_votes = world
            .rpc
            .finalized_ocomp_result_vote_transactions_on(
                primary,
                request.request_height,
                finalized_height,
            )
            .unwrap_or_default();
        if let Some(transaction) = public_votes.iter().find(|transaction| transaction.success) {
            break world
                .rpc
                .ocomp_result_vote_bytes_on(primary, transaction.transaction_hash)
                .expect("decode one finalized public ResultVoteV1");
        }
        assert!(
            Instant::now() < timeout,
            "no valid public result vote became available for held-vote preparation"
        );
        sleep(Duration::from_millis(250));
    };
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical public ResultVoteV1");
    let address = world
        .ocomp
        .ocomp_delegate_address(VALIDATOR_INDEX as u8)
        .expect("derive validator-2 OCOMP delegate address");
    let nonce = world
        .rpc
        .canonical_nonce_on(primary, address)
        .expect("read validator-2 canonical nonce");
    let max_fee_per_gas = world
        .rpc
        .gas_price_on(primary)
        .expect("read public gas price")
        .max(MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS);
    assert!(
        world
            .rpc
            .head(primary)
            .expect("primary head before preparing held vote")
            < request.deadline_height,
        "held vote was not prepared before the exclusive deadline"
    );
    let prepared = world
        .ocomp
        .prepare_held_vote_transaction(
            VALIDATOR_INDEX as u8,
            vote,
            nonce,
            max_fee_per_gas,
            OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        )
        .expect("production node prepares validator-2 vote without exposing its key");
    world.state.ocomp_held_late_vote_hash = Some(prepared.transaction_hash);
    world.state.ocomp_held_late_vote_raw = Some(prepared.raw_transaction.0);
}

#[when("the held validator vote is broadcast at the exclusive deadline")]
fn held_vote_is_broadcast_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let target_parent = request
        .deadline_height
        .checked_sub(1)
        .expect("deadline follows genesis");
    let timeout = Instant::now() + Duration::from_secs(180);
    while world
        .rpc
        .head(primary)
        .expect("primary head while approaching exclusive vote deadline")
        < target_parent
    {
        assert!(
            Instant::now() < timeout,
            "chain did not approach the exclusive vote deadline"
        );
        sleep(Duration::from_millis(50));
    }

    let raw = world
        .state
        .ocomp_held_late_vote_raw
        .take()
        .expect("locally signed held vote transaction");
    let expected_hash = world
        .state
        .ocomp_held_late_vote_hash
        .expect("held vote transaction hash");
    let submitted_hash = world
        .rpc
        .send_raw_transaction_on(primary, &raw)
        .expect("broadcast held vote through public RPC");
    assert_eq!(
        submitted_hash
            .parse::<B256>()
            .expect("public transaction hash"),
        expected_hash,
        "public RPC changed the locally signed held transaction identity"
    );
    let receipt_timeout = Instant::now() + Duration::from_secs(60);
    let receipt = loop {
        if let Some(receipt) = world
            .rpc
            .transaction_receipt(&submitted_hash, primary)
            .filter(|receipt| receipt.get("blockNumber").is_some())
        {
            break receipt;
        }
        assert!(
            Instant::now() < receipt_timeout,
            "held vote did not receive a mined public receipt"
        );
        sleep(Duration::from_millis(100));
    };
    assert_eq!(
        receipt.get("status").and_then(serde_json::Value::as_str),
        Some("0x0"),
        "a vote included at or after the exclusive deadline must revert"
    );
    let inclusion_height = world
        .rpc
        .receipt_block_number(&submitted_hash, primary)
        .expect("held vote inclusion height");
    assert!(
        inclusion_height >= request.deadline_height,
        "held vote was included before the intended exclusive boundary: \
         inclusion={inclusion_height}, deadline={}",
        request.deadline_height
    );
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, inclusion_height, 60),
        "held deadline-vote receipt did not finalize"
    );
    world.state.ocomp_late_vote_reverted = Some(true);
    world.state.ocomp_late_vote_inclusion_height = Some(inclusion_height);
}

#[then("three matching validator domains atomically apply Lysis and create the Nod")]
fn quorum_applies_lysis_and_creates_nod(world: &mut World) {
    quorum_applies_lysis_and_creates_nod_with_vote_expectation(
        world,
        PublicVoteSetExpectation::AnyQuorum,
    );
}

#[then("Lysis and OCOMP use the WWD VWAP below the active S-curve")]
fn lysis_and_ocomp_use_wwd_below_scurve(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized OCOMP activation");
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod generation");
    let actions = result_nod_actions_on(world, 0, generation.job_id);
    let [action] = actions.as_slice() else {
        panic!("single-Tribute pricing scenario must produce exactly one Nod action")
    };
    let (wwd_vwap, scurve) = world
        .rpc
        .oracle_wwd_vwap_and_scurve(
            world.validators.primary_port(),
            activation.worldwide_day,
            840,
        )
        .expect("read canonical WWD VWAP and active S-curve");
    assert!(
        scurve > wwd_vwap,
        "fixture must keep an active S-curve above the WWD VWAP"
    );
    assert_eq!(
        action.entry_price_minor, wwd_vwap,
        "Lysis/Nod must carry WWD VWAP rather than the higher S-curve"
    );
}

#[then("three compatible validator domains atomically apply Lysis and create the Nod")]
fn compatible_quorum_applies_lysis_and_creates_nod(world: &mut World) {
    quorum_applies_lysis_and_creates_nod_with_vote_expectation(
        world,
        PublicVoteSetExpectation::Exact(&[1, 2, 3]),
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicVoteSetExpectation {
    AnyQuorum,
    Exact(&'static [u16]),
}

fn public_vote_set_matches(
    expectation: PublicVoteSetExpectation,
    validator_indexes: &[u16],
    quorum_threshold: usize,
) -> bool {
    match expectation {
        PublicVoteSetExpectation::AnyQuorum => validator_indexes.len() >= quorum_threshold,
        PublicVoteSetExpectation::Exact(expected) => validator_indexes == expected,
    }
}

fn completed_accountability_is_preserved(
    expected: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    observed: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
) -> bool {
    expected.job_id == observed.job_id
        && expected.result_validator_set_epoch == observed.result_validator_set_epoch
        && expected.result_committee_set_hash == observed.result_committee_set_hash
        && expected.result_ocomp_binding_hash == observed.result_ocomp_binding_hash
        && expected.member_count == observed.member_count
        && expected.quorum_threshold == observed.quorum_threshold
        && expected.quorum_result_digest == observed.quorum_result_digest
        && expected.quorum_height == observed.quorum_height
        && expected.quorum_signer_bitmap == observed.quorum_signer_bitmap
        && expected
            .slot_validator_indexes
            .iter()
            .all(|index| observed.slot_validator_indexes.contains(index))
        && expected
            .slot_first_signatures
            .iter()
            .all(|signature| observed.slot_first_signatures.contains(signature))
}

fn quorum_applies_lysis_and_creates_nod_with_vote_expectation(
    world: &mut World,
    vote_expectation: PublicVoteSetExpectation,
) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    quorum_applies_lysis_and_creates_nod_for_request(world, request, vote_expectation);
}

fn quorum_applies_lysis_and_creates_nod_for_request(
    world: &mut World,
    request: crate::world::rpc::OcompPublicJobRequestV1,
    vote_expectation: PublicVoteSetExpectation,
) {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let ports = world.validators.committee_ports();
        let activations = ports
            .iter()
            .copied()
            .map(|port| {
                world.rpc.finalized_ocomp_activation_on(
                    port,
                    request.request_height,
                    request.intent_id,
                )
            })
            .collect::<Vec<_>>();
        if activations.iter().all(Option::is_some) {
            let activation = activations[0].clone().expect("all activations are present");
            assert!(
                activations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&activation)),
                "validators expose different finalized Lysis activation"
            );
            assert_eq!(activation.intent_id, request.intent_id);
            assert_eq!(activation.worldwide_day, request.worldwide_day);
            assert_ne!(activation.job_id, B256::ZERO);
            assert_ne!(activation.result_digest, B256::ZERO);
            assert_ne!(activation.activation_call_id, B256::ZERO);
            assert_ne!(activation.terminal_receipt_hash, B256::ZERO);

            let generations = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized_ocomp_certified_generation_on(port, &activation)
                })
                .collect::<Vec<_>>();
            assert!(
                generations.iter().all(Option::is_some),
                "one or more validators cannot verify both generation projections at the exact \
                 finalized activation block: {generations:?}"
            );
            let generation = generations[0]
                .clone()
                .expect("all certified generations are present");
            assert!(
                generations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&generation)),
                "validators expose different certified Nod generations"
            );
            assert_eq!(generation.worldwide_day, request.worldwide_day);
            assert_eq!(generation.job_id, activation.job_id);
            assert_eq!(generation.block_number, activation.block_number);
            assert_eq!(generation.block_hash, activation.block_hash);
            assert_ne!(generation.program_semantics_hash, B256::ZERO);
            assert_ne!(generation.nod_root, B256::ZERO);
            assert_ne!(generation.bucket_root, B256::ZERO);
            assert_ne!(generation.output_manifest_root, B256::ZERO);
            assert_eq!(generation.tribute_count, generation.nod_count);
            assert!(generation.tribute_count > 0);
            assert!(generation.bucket_count <= generation.nod_count);

            let accountability_deadline = Instant::now() + Duration::from_secs(120);
            let accountability = loop {
                let observed = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_vote_accountability_on(port, activation.job_id)
                    })
                    .collect::<Vec<_>>();
                if observed.iter().all(|value| {
                    value.as_ref().is_some_and(|accountability| {
                        public_vote_set_matches(
                            vote_expectation,
                            &accountability.slot_validator_indexes,
                            usize::from(accountability.quorum_threshold),
                        )
                    })
                }) {
                    let first = observed[0]
                        .clone()
                        .expect("all accountability records are present");
                    assert!(
                        observed.iter().all(|value| value.as_ref() == Some(&first)),
                        "validators expose different finalized vote accountability"
                    );
                    break first;
                }
                assert!(
                    Instant::now() < accountability_deadline,
                    "the expected {vote_expectation:?} validator vote set did not reach \
                     finalized accountability: {observed:?}"
                );
                sleep(Duration::from_millis(250));
            };
            assert_eq!(accountability.job_id, activation.job_id);
            let observed_vote_count = accountability.slot_validator_indexes.len();
            assert_eq!(
                accountability.quorum_result_digest,
                Some(activation.result_digest)
            );
            assert_eq!(
                accountability
                    .quorum_signer_bitmap
                    .as_ref()
                    .expect("completed job quorum")
                    .iter()
                    .map(|byte| byte.count_ones())
                    .sum::<u32>(),
                3
            );

            let finalized_height = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized(port)
                        .expect("validator finalized height")
                })
                .min()
                .expect("four validator ports");
            let public_votes = ports
                .iter()
                .copied()
                .map(|port| {
                    world.rpc.finalized_ocomp_result_vote_transactions_on(
                        port,
                        request.request_height,
                        finalized_height,
                    )
                })
                .collect::<Vec<_>>();
            assert!(
                public_votes.iter().all(Option::is_some),
                "one or more validators cannot enumerate finalized public result votes"
            );
            let first_votes = public_votes[0]
                .clone()
                .expect("all public vote collections are present");
            assert!(
                public_votes
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&first_votes)),
                "proposer/import/replay validators expose different public result-vote transactions"
            );
            assert_eq!(
                first_votes
                    .iter()
                    .filter(|transaction| transaction.success)
                    .count(),
                observed_vote_count,
                "unexpected number of independent successful public validator result votes"
            );
            let mut signers = first_votes
                .iter()
                .map(|transaction| transaction.signer)
                .collect::<Vec<_>>();
            signers.sort_unstable();
            signers.dedup();
            assert_eq!(
                signers.len(),
                observed_vote_count,
                "public result votes must come from the expected distinct validator EVM signers"
            );
            assert!(
                first_votes
                    .iter()
                    .any(|transaction| transaction.transaction_hash == activation.transaction_hash),
                "q-forming activation transaction is absent from the public result-vote set"
            );

            let primary = world.validators.primary_port();
            let mut balances_after = Vec::with_capacity(4);
            for (address, before) in &world.state.ocomp_validator_balances_before {
                let after = world
                    .rpc
                    .balance_on(primary, &format!("{address:#x}"))
                    .expect("read validator balance after result votes");
                assert_eq!(
                    after, *before,
                    "OCOMP delegate {address:#x} paid for a system-carrier result vote"
                );
                balances_after.push((*address, after));
            }

            if generation.tribute_count as usize == OCOMP_CAPACITY_TRIBUTE_COUNT {
                let q_forming = first_votes
                    .iter()
                    .find(|transaction| transaction.transaction_hash == activation.transaction_hash)
                    .expect("q-forming public transaction");
                let vote_bytes = world
                    .rpc
                    .ocomp_result_vote_bytes_on(primary, q_forming.transaction_hash)
                    .expect("canonical q-forming ResultVoteV1 bytes");
                let internal_work =
                    outbe_ocomp_protocol::capacity::result_vote_internal_work(vote_bytes.len())
                        .expect("q-forming vote fits generated internal-work cap");
                let finalized_block_hash = world
                    .rpc
                    .block_hash(primary, finalized_height)
                    .and_then(|value| value.parse::<B256>().ok())
                    .expect("finalized capacity capture block hash");
                let finality_latency_micros = ports
                    .iter()
                    .enumerate()
                    .map(|(validator_index, _)| {
                        world
                            .localnet
                            .validator_finality_latency_micros(
                                validator_index,
                                q_forming.block_number,
                                q_forming.block_hash,
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "observe q-forming capacity finality on validator \
                                     {validator_index}: {error:#}"
                                )
                            })
                    })
                    .max()
                    .expect("four validator finality observations");
                let block_processing_micros_by_validator = ports
                    .iter()
                    .enumerate()
                    .map(|(validator_index, _)| {
                        world
                            .localnet
                            .validator_block_processing_micros(
                                validator_index,
                                q_forming.block_number,
                                q_forming.block_hash,
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "observe q-forming capacity block on validator \
                                     {validator_index}: {error:#}"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let block_processing_micros = block_processing_micros_by_validator
                    .iter()
                    .copied()
                    .max()
                    .expect("four validator block-processing timings");
                let block_commitments = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .block_commitment(port, q_forming.block_number)
                            .unwrap_or_else(|| {
                                panic!(
                                    "validator on port {port} has no canonical q-forming \
                                     block/state/CE commitment"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let canonical_commitment = block_commitments
                    .first()
                    .expect("four validator block commitments");
                assert_eq!(
                    canonical_commitment.block_hash, q_forming.block_hash,
                    "receipt block hash differs from the canonical imported block"
                );
                assert!(
                    block_commitments
                        .iter()
                        .all(|observed| observed == canonical_commitment),
                    "validators imported different q-forming block/state/CE commitments: \
                     {block_commitments:?}"
                );
                let receipt_hash = format!("{:#x}", q_forming.transaction_hash);
                let receipts = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .transaction_receipt(&receipt_hash, port)
                            .unwrap_or_else(|| {
                                panic!(
                                    "validator on port {port} has no canonical q-forming receipt"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let canonical_receipt =
                    receipts.first().expect("four validator q-forming receipts");
                assert!(
                    receipts
                        .iter()
                        .all(|observed| observed == canonical_receipt),
                    "validators retained different q-forming receipts"
                );
                let q_forming_validator_receipt_sha256 = receipts
                    .iter()
                    .map(|receipt| {
                        crate::ocomp_evidence::sha256_hex(
                            &serde_json::to_vec(receipt)
                                .expect("canonical q-forming receipt is JSON-serializable"),
                        )
                    })
                    .collect::<Vec<_>>();
                let q_forming_receipt_sha256 = q_forming_validator_receipt_sha256
                    .first()
                    .expect("four validator q-forming receipt digests")
                    .clone();
                world.state.ocomp_capacity_observation =
                    Some(crate::world::state::OcompPublicCapacityObservationV1 {
                        job_id: activation.job_id,
                        result_digest: activation.result_digest,
                        q_forming_transaction_hash: q_forming.transaction_hash,
                        q_forming_block_number: q_forming.block_number,
                        q_forming_block_hash: q_forming.block_hash,
                        q_forming_receipt_success: q_forming.success,
                        q_forming_receipt_sha256,
                        q_forming_validator_receipt_sha256,
                        q_forming_state_root: canonical_commitment.state_root,
                        q_forming_ce_root: canonical_commitment.ce_root,
                        q_forming_validator_commitments: block_commitments.clone(),
                        canonical_import_validator_count: u8::try_from(
                            block_commitments.len(),
                        )
                        .expect("validator count fits u8"),
                        canonical_import_verified: true,
                        finalized_block_number: finalized_height,
                        finalized_block_hash,
                        tribute_count: u64::from(generation.tribute_count),
                        nod_count: u64::from(generation.nod_count),
                        worker_shard_count:
                            outbe_ocomp_protocol::capacity::worker_shard_count(
                                u64::from(generation.tribute_count),
                                u32::try_from(
                                    outbe_ocomp_protocol::generated_shape::
                                        OCOMP_POC_CANDIDATE_LIMITS_V1
                                            .max_tributes_per_work_shard,
                                )
                                .expect("generated shard cap fits u32"),
                            )
                            .expect("generated shard cap is non-zero"),
                        transaction_bytes: u64::try_from(q_forming.raw_transaction_len)
                            .expect("q-forming transaction length fits u64"),
                        block_bytes: u64::try_from(q_forming.block_rlp_len)
                            .expect("q-forming block length fits u64"),
                        gas: q_forming.gas_used,
                        internal_work,
                        block_processing_micros_by_validator,
                        block_processing_micros,
                        finality_latency_micros,
                    });
            }

            world.state.ocomp_activation = Some(activation);
            world.state.ocomp_certified_generation = Some(generation);
            world.state.ocomp_result_vote_transactions = first_votes;
            world.state.ocomp_vote_accountability = Some(accountability);
            world.state.ocomp_validator_balances_after = balances_after;
            world.state.ocomp_atomic_quorum_apply_verified = true;
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive through public activation");
        assert!(
            Instant::now() < deadline,
            "q=3 public Lysis activation did not finalize before the bounded E2E deadline; \
             activations={activations:?}"
        );
        sleep(Duration::from_millis(500));
    }
}

#[then("the keyless FullNode verifies the same finalized Nod body through its local proof path")]
fn keyless_full_node_verifies_finalized_nod_body(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized OCOMP activation before FullNode Nod proof read");
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod generation before FullNode proof read");
    assert_eq!(generation.nod_count, 1, "single-Tribute proof scenario");

    let primary = world.validators.primary_port();
    let full_node = world.validators.http_port(world.validators.joiner_index());
    assert!(
        world
            .rpc
            .wait_finalized_at_least(full_node, activation.block_number, 60),
        "keyless FullNode did not finalize the canonical Nod activation block"
    );
    assert_eq!(
        world.rpc.state_root(full_node, activation.block_number),
        world.rpc.state_root(primary, activation.block_number),
        "FullNode EVM state root differs at Nod activation"
    );
    assert_eq!(
        world
            .rpc
            .finalized_ocomp_certified_generation_on(full_node, activation),
        Some(generation.clone()),
        "FullNode exposes a different certified Nod generation"
    );

    let tribute_owner_key = world
        .validators
        .by_name("validator-0")
        .expect("public Tribute owner")
        .evm_key()
        .expect("public Tribute owner key");
    let tribute_owner = eth::address_of(&tribute_owner_key).expect("public Tribute owner address");
    let validator_actions = result_nod_actions_on(world, 0, generation.job_id);
    let full_node_actions =
        result_nod_actions_on(world, world.validators.joiner_index(), generation.job_id);
    assert_eq!(full_node_actions, validator_actions);
    let [action] = full_node_actions.as_slice() else {
        panic!("single-Tribute proof scenario must materialize exactly one Nod action")
    };
    assert_eq!(action.owner, tribute_owner);
    assert_eq!(action.wwd, activation.worldwide_day);
    let authority = ActiveNodSetV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_root: generation.nod_root,
        nod_count: generation.nod_count,
    };
    NodMembershipProofV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_ordinal: 0,
        action: action.clone(),
        membership_siblings: Vec::new(),
    }
    .verify_against(&authority, &poc_schema_limits())
    .expect("FullNode Nod action membership proof against finalized generation root");

    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("public JobIntent before FullNode local-result comparison");
    let local_result = std::fs::read(full_node_local_result_path(world, generation.job_id))
        .expect("read keyless FullNode canonical Lysis result");
    let local_result = LysisResultV1::decode_canonical(&local_result, &poc_schema_limits())
        .expect("decode keyless FullNode canonical Lysis result");
    assert_eq!(local_result.job_id, generation.job_id);
    assert_eq!(request.intent_id, activation.intent_id);
    assert_eq!(
        local_result
            .result_digest(&poc_schema_limits())
            .expect("validate keyless FullNode canonical Lysis result"),
        activation.result_digest,
        "FullNode computed result differs from the canonical quorum result"
    );
    assert!(
        !dynamic_vote_submission_path(world, world.validators.joiner_index(), generation.job_id,)
            .exists(),
        "keyless FullNode must not publish an OCOMP vote"
    );
}

#[then("the certified generation contains exactly 257 Tribute and Nod records")]
fn certified_generation_contains_257_records(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("capacity certified generation");
    assert_eq!(
        generation.tribute_count,
        u32::try_from(OCOMP_CAPACITY_TRIBUTE_COUNT).expect("capacity count fits u32")
    );
    assert_eq!(
        generation.nod_count,
        u32::try_from(OCOMP_CAPACITY_TRIBUTE_COUNT).expect("capacity count fits u32")
    );
    assert_eq!(
        outbe_ocomp_protocol::capacity::worker_shard_count(
            u64::from(generation.tribute_count),
            u32::try_from(
                outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                    .max_tributes_per_work_shard
            )
            .expect("generated shard cap fits u32"),
        )
        .expect("non-zero generated shard cap"),
        2,
        "the public S+1 population must be covered by two worker shards"
    );
    let fresh_worldwide_day = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .map(|_| fresh_metadosis_wwd(world));
    if let Some(worldwide_day) = fresh_worldwide_day {
        let finalized_height = world
            .state
            .ocomp_capacity_observation
            .as_ref()
            .expect("fresh capacity public-path observation")
            .finalized_block_number;
        let completed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .metadosis_wwd_state_at(port, worldwide_day, finalized_height)
            })
            .collect::<Vec<_>>();
        assert!(
            completed
                .iter()
                .all(|state| state.as_ref().is_some_and(|state| state.status == 6)),
            "the runtime-created fresh WWD is not COMPLETED on every validator"
        );
        if let Some(lifecycle) = world.state.metadosis_fresh_lifecycle_observation.as_mut() {
            lifecycle.completed_validator_count = 4;
        }
    }
}

#[then("validator 0 reconstructs that certified generation from canonical history")]
fn validator_zero_reconstructs_certified_generation(world: &mut World) {
    let capacity = world
        .state
        .ocomp_capacity_observation
        .clone()
        .expect("capacity public-path observation");
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("capacity finalized JobIntent");
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("capacity finalized activation");
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("capacity certified generation");
    let recovery = world
        .localnet
        .reconstruct_validator_ce_from_canonical_history(0)
        .unwrap_or_else(|error| {
            panic!("reconstruct validator-0 CE from canonical history: {error:#}")
        });
    assert!(
        recovery.first_missing_block_number <= capacity.q_forming_block_number
            && recovery.target_block_number >= capacity.finalized_block_number,
        "historical CE replay span does not cover the q-forming/finalized capacity blocks: \
         recovery={recovery:?}, capacity={capacity:?}"
    );
    assert_eq!(
        recovery.replayed_block_count,
        recovery.target_block_number - recovery.first_missing_block_number + 1
    );

    let primary = world.validators.primary_port();
    let canonical_target_hash = world
        .rpc
        .block_hash(primary, recovery.target_block_number)
        .and_then(|value| value.parse::<B256>().ok())
        .expect("restarted validator exposes replay target block");
    assert_eq!(
        canonical_target_hash, recovery.target_block_hash,
        "startup replay target is not the restarted validator's canonical block"
    );

    let deadline = Instant::now() + Duration::from_secs(120);
    let (recovered_activation, recovered_generation) = loop {
        let recovered_activation = world.rpc.finalized_ocomp_activation_on(
            primary,
            request.request_height,
            request.intent_id,
        );
        let recovered_generation = recovered_activation.as_ref().and_then(|observed| {
            world
                .rpc
                .finalized_ocomp_certified_generation_on(primary, observed)
        });
        if let (Some(recovered_activation), Some(recovered_generation)) =
            (recovered_activation, recovered_generation)
        {
            break (recovered_activation, recovered_generation);
        }
        assert!(
            Instant::now() < deadline,
            "restarted validator did not expose the recovered certified generation"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(
        recovered_activation, activation,
        "historical CE replay changed the finalized activation"
    );
    assert_eq!(
        recovered_generation, generation,
        "historical CE replay changed the certified generation"
    );
    world.state.ocomp_historical_replay_observation =
        Some(crate::world::state::OcompHistoricalReplayObservationV1 {
            recovery,
            recovered_result_digest: recovered_activation.result_digest,
            recovered_generation,
        });
}

#[when("the completed full-result vote is retried and then mutated through public RPC")]
fn completed_vote_is_retried_and_mutated(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("completed public Lysis activation");
    let primary = world.validators.primary_port();
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, activation.transaction_hash)
        .expect("decode the q-forming public result vote");
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical q-forming ResultVoteV1");
    assert_eq!(vote.job_id, activation.job_id);

    let delegate_key = world
        .ocomp
        .ocomp_delegate_private_key_for_vote(&vote)
        .expect("q-forming vote OCOMP delegate key");

    let retry_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &delegate_key, vote_bytes)
        .expect("submit exact completed-vote retry through public RPC");
    let retry_receipt = world
        .rpc
        .transaction_receipt(&retry_hash, primary)
        .expect("exact retry receipt");
    assert_eq!(
        retry_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x1"),
        "exact completed-vote retry must be idempotently accepted"
    );
    world.state.ocomp_exact_completed_retry_succeeded = Some(true);

    let mut mutated = vote;
    mutated.job_id = B256::repeat_byte(0xa5);
    let mutated_bytes = mutated
        .encode_canonical(&poc_schema_limits())
        .expect("structurally canonical changed-binding vote");
    let mutation = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &delegate_key, mutated_bytes);
    let mutation_block = match mutation {
        Ok(mutation_hash) => {
            let mutation_receipt = world
                .rpc
                .transaction_receipt(&mutation_hash, primary)
                .expect("changed-binding receipt");
            assert_eq!(
                mutation_receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str),
                Some("0x0"),
                "changed-binding completed vote must revert in the OCOMP module"
            );
            Some(
                world
                    .rpc
                    .receipt_block_number(&mutation_hash, primary)
                    .expect("changed-binding block"),
            )
        }
        Err(error) => {
            assert!(
                error
                    .to_string()
                    .contains("OCOMP carrier signer is not authorized for this action"),
                "changed-binding vote failed for an unexpected reason: {error:#}"
            );
            None
        }
    };
    world.state.ocomp_changed_completed_binding_reverted = Some(true);

    let retry_block = world
        .rpc
        .receipt_block_number(&retry_hash, primary)
        .expect("exact retry block");
    let finality_target = mutation_block.map_or(retry_block, |height| retry_block.max(height));
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, finality_target, 60),
        "public retry/mutation receipts did not finalize"
    );
}

#[then("the completed job and Nod generation are unchanged by both transactions")]
fn completed_job_and_generation_are_unchanged(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized public JobIntent");
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("completed public activation");
    let expected_accountability = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("four-slot accountability before retry");
    let expected_generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified generation before retry");

    for port in world.validators.committee_ports() {
        let record = world
            .rpc
            .finalized_ocomp_job_record_on(port, request.intent_id)
            .expect("completed job record after public retry/mutation");
        let completed = record
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.completed_binding.as_ref())
            .expect("completed binding remains present");
        assert_eq!(completed.job_id, activation.job_id);
        assert_eq!(completed.result_digest, activation.result_digest);
        assert_eq!(
            completed.terminal_receipt_hash,
            activation.terminal_receipt_hash
        );

        let accountability = world
            .rpc
            .finalized_ocomp_vote_accountability_on(port, activation.job_id)
            .expect("accountability after public retry/mutation");
        assert!(
            completed_accountability_is_preserved(expected_accountability, &accountability),
            "completed accountability binding/quorum changed after public retry/mutation: \
             expected={expected_accountability:?} observed={accountability:?}"
        );

        let generation = world
            .rpc
            .finalized_ocomp_certified_generation_on(port, activation)
            .expect("certified generation after public retry/mutation");
        assert_eq!(&generation, expected_generation);
    }
    world.state.ocomp_completed_state_unchanged = Some(true);
}

#[when("validators 2 and 3 OCOMP workers are stopped before the job")]
fn stop_two_workers_before_job(world: &mut World) {
    capture_ocomp_finality_before_fault(world, "stopping OCOMP workers");
    for validator_index in [2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 0,
            })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Worker before the job: {error}")
            });
    }
}

fn capture_ocomp_finality_before_fault(world: &mut World, action: &str) {
    let ports = world.validators.committee_ports();
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .wait_finalized_checkpoint(&ports, 1, 60)
            .unwrap_or_else(|error| {
                panic!("capture common finalized checkpoint before {action}: {error:#}")
            })
            .height,
    );
}

#[when("all four OCOMP snapshot exporters are stopped before the job")]
fn stop_all_exporters_before_job(world: &mut World) {
    capture_ocomp_finality_before_fault(world, "stopping all OCOMP snapshot exporters");
    for validator_index in 0..4 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index })
            .unwrap_or_else(|error| {
                panic!(
                    "stop validator-{validator_index} SnapshotExporter before the job: {error:#}"
                )
            });
    }
}

#[when(
    "all four OCOMP workers stop before voting opens and exporters independently materialize the public JobIntent"
)]
fn stop_workers_before_independent_public_exports(world: &mut World) {
    use crate::internal::ocomp_worker_outage::{require_pre_open_cut, WorkerOutageEvidence};
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all four workers live and connected before observing the job");
    let ports = world.validators.committee_ports();
    let current_finalized = ports
        .iter()
        .map(|&port| world.rpc.finalized_result(port))
        .collect::<eyre::Result<Vec<_>>>()
        .expect("read current finalized heights before worker outage")
        .into_iter()
        .min()
        .expect("four-validator committee");
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .wait_finalized_checkpoint(&ports, current_finalized, 60)
            .expect("current common finalized checkpoint before worker outage")
            .height,
    );
    world.state.ocomp_worker_outage = Some(WorkerOutageEvidence::default());
    world
        .ocomp
        .stop_worker_cohort(world.state.ocomp_worker_outage.as_mut().unwrap())
        .expect("stop and reap all four owned workers before observing exports");
    // Capture the cut immediately after reaping. Later export publication cannot
    // move this boundary; its independence from workers is the property tested.
    let heads = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            let raw = eth::raw_json_result(
                &world.rpc.url(port),
                "eth_blockNumber",
                serde_json::json!([]),
            )
            .expect("successful canonical HEAD after worker reap");
            serde_json::from_value::<alloy_primitives::U64>(raw)
                .expect("canonical HEAD number")
                .to::<u64>()
        })
        .collect::<Vec<_>>();
    world.state.ocomp_worker_outage.as_mut().unwrap().cut_heads = heads.clone();
    metadosis_creates_finalized_job_intent(world);
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized zero-vote JobIntent");
    require_pre_open_cut(&heads, request.open_height)
        .expect("all four workers must exit before voting opens");
    let primary = world.validators.primary_port();
    let record = world
        .rpc
        .ocomp_job_record_at_on(primary, request.intent_id, request.finality_recorded_height)
        .expect("canonical bound job for exact export verification");
    assert_eq!(record.finalized.as_ref().unwrap().job_id, request.job_id);
    let checkpoint = world
        .rpc
        .checkpoint_at(primary, request.request_height)
        .expect("canonical request checkpoint");
    assert_eq!(checkpoint.block_hash, request.request_block_hash);
    let bundle = world
        .ocomp
        .canonical_fork_install()
        .expect("canonical OCOMP bundle")
        .protocol_bundle;
    let opening_checkpoint =
        wait_for_common_finalized_checkpoint(world, request.open_height, "worker outage opening");
    assert_eq!(opening_checkpoint.height, request.open_height);
    world
        .ocomp
        .wait_for_exports_while_workers_stopped(
            &record,
            checkpoint,
            &bundle,
            world.state.ocomp_worker_outage.as_mut().unwrap(),
            Duration::from_secs(120),
            || world.localnet.ensure_committee_alive(),
        )
        .unwrap_or_else(|error| {
            panic!("exact exports while the complete worker cohort remains stopped: {error:#}")
        });
    for port in world.validators.committee_ports() {
        // Accountability is created at open, not at the earlier worker cut.
        let accountability = world
            .rpc
            .ocomp_vote_accountability_at_on(port, request.job_id, opening_checkpoint.height)
            .expect("canonical post-fault accountability");
        assert!(accountability.slot_validator_indexes.is_empty());
        assert!(accountability.quorum_result_digest.is_none());
    }
}

#[when("validator 3 OCOMP worker is stopped before the job")]
fn stop_late_worker_before_job(world: &mut World) {
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all baseline OCOMP workers are live and authenticated before the fault");
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopWorker {
            validator_index: 3,
            worker_ordinal: 0,
        })
        .expect("stop validator-3 Worker before the job");
}

#[then("validators 0, 1 and 2 finalize the result quorum while validator 3 remains computing")]
fn three_validators_finalize_before_late_compute(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(600);

    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validator committee remains alive before the late local result");
        for validator_index in [0, 1, 2] {
            world
                .ocomp
                .ensure_worker_alive(validator_index, 0)
                .unwrap_or_else(|error| {
                    panic!(
                        "validator-{validator_index} Worker exited before the three-voter quorum: {error}"
                    )
                });
        }
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        let completed = records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::Completed)
        });
        if completed {
            let record = records[0]
                .clone()
                .expect("all completed records are present");
            assert!(
                records
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&record)),
                "validators expose different completed OCOMP records"
            );
            let finalized = record
                .finalized
                .as_ref()
                .expect("completed OCOMP record is finalized");
            let job_id = finalized.job_id;
            let accountability = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id)
                .expect("completed three-voter accountability");
            if accountability.slot_validator_indexes != [0, 1, 2] {
                assert!(
                    Instant::now() < timeout,
                    "unexpected finalized OCOMP voters before late compute: {:?}",
                    accountability.slot_validator_indexes
                );
                sleep(Duration::from_millis(250));
                continue;
            }
            assert!(accountability.quorum_result_digest.is_some());
            assert!(
                !local_result_path(world, 3, job_id).exists(),
                "validator-3 produced a local result while its Worker was stopped"
            );
            let activations = ports
                .iter()
                .copied()
                .map(|port| {
                    world.rpc.finalized_ocomp_activation_on(
                        port,
                        request.request_height,
                        request.intent_id,
                    )
                })
                .collect::<Vec<_>>();
            assert!(activations.iter().all(Option::is_some));
            let activation = activations[0]
                .clone()
                .expect("all finalized activations are present");
            assert!(
                activations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&activation)),
                "validators expose different finalized activations"
            );
            world.state.ocomp_activation = Some(activation);
            world.state.ocomp_vote_accountability = Some(accountability);
            world.state.ocomp_finality_before_fault = Some(
                world
                    .rpc
                    .finalized_result(primary)
                    .expect("capture finalized height before releasing held OCOMP result"),
            );
            return;
        }
        assert!(
            Instant::now() < timeout,
            "three validators did not finalize the OCOMP quorum while validator-3 was held"
        );
        sleep(Duration::from_millis(250));
    }
}

#[when("validator 3 OCOMP worker restarts after the finalized quorum")]
fn restart_late_worker_after_quorum(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("canonical activation");
    let job_id = activation.job_id;
    let canonical_result = std::fs::read(local_result_path(world, 0, job_id))
        .expect("read the quorum participant's canonical result");
    let result = LysisResultV1::decode_canonical(&canonical_result, &poc_schema_limits())
        .expect("decode the quorum participant's canonical result");
    assert_eq!(result.job_id, job_id);
    assert_eq!(
        result.result_digest(&poc_schema_limits()).unwrap(),
        activation.result_digest
    );
    assert_eq!(
        world
            .state
            .ocomp_vote_accountability
            .as_ref()
            .unwrap()
            .quorum_result_digest,
        Some(activation.result_digest)
    );
    let (cas, reference) = late_validator_result_cas(world, &canonical_result);
    assert!(
        read_pending_late_result(&cas, &reference).is_none(),
        "held worker must not have already produced this job's exact result"
    );
    assert!(!local_result_path(world, 3, job_id)
        .try_exists()
        .expect("observe held validator's local result path"));
    let node_dir = world.validators.data_dir(3);
    let node_log = crate::internal::launch_log::LaunchLog::checkpoint(
        &node_dir
            .parent()
            .expect("validator slot directory")
            .join("node.log"),
    )
    .expect("arm the existing node log before the late worker starts");
    let (node_pid, status) = world
        .localnet
        .owned_validator_process(3)
        .expect("capture exact live validator-3 incarnation");
    assert!(status.is_none());
    world
        .ocomp
        .restart_worker(3, 0)
        .expect("restart validator-3 Worker after finalized quorum");
    let worker_pid = late_validator_worker_pid(world);
    world.state.ocomp_late_validator_result =
        Some(crate::world::state::LateValidatorResultObservation {
            node_pid,
            worker_pid,
            node_log,
            canonical_result,
        });
}

fn late_validator_worker_pid(world: &World) -> u32 {
    let workers = world
        .ocomp
        .process_records()
        .iter()
        .filter(|record| {
            record.validator_index == Some(3)
                && record.role == OcompProcessRole::Worker
                && record.worker_ordinal == Some(0)
                && record.stopped_at_millis.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        workers.len(),
        1,
        "late worker must have one exact owned incarnation"
    );
    workers[0].pid
}

fn late_validator_result_cas(
    world: &World,
    canonical_result: &[u8],
) -> (
    outbe_ocomp::cas::FilesystemCasReader,
    outbe_ocomp_protocol::CasObjectRefV1,
) {
    let encoded_bytes = u64::try_from(canonical_result.len()).expect("canonical result length");
    let cas = outbe_ocomp::cas::FilesystemCasReader::open(
        world
            .ocomp
            .domain_root(3)
            .expect("validator-3 domain")
            .join("cas-v1"),
        outbe_ocomp::cas::CasLimits {
            // This observer reads one exact expected object, not a job-data quota.
            max_object_bytes: encoded_bytes,
            max_total_bytes: u64::MAX,
        },
    )
    .expect("open the held validator's existing CAS read-only");
    let reference = outbe_ocomp_protocol::CasObjectRefV1 {
        transport_digest: alloy_primitives::keccak256(canonical_result),
        encoded_bytes,
        expected_ocb1_kind: None,
    };
    (cas, reference)
}

fn read_pending_late_result(
    cas: &outbe_ocomp::cas::FilesystemCasReader,
    reference: &outbe_ocomp_protocol::CasObjectRefV1,
) -> Option<Vec<u8>> {
    match cas.read_verified(reference) {
        Ok(object) => Some(object.bytes().to_vec()),
        Err(outbe_ocomp::cas::CasError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => panic!("late result CAS observation failed: {error}"),
    }
}

#[then("validator 3 safely handles its correct late result without changing the canonical outcome or votes")]
fn late_local_result_is_not_fatal(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("finalized OCOMP activation");
    let job_id = activation.job_id;
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("canonical request");
    let mut observation = world
        .state
        .ocomp_late_validator_result
        .take()
        .expect("late result observer was armed before restarting the worker");
    let (cas, reference) = late_validator_result_cas(world, &observation.canonical_result);
    let result_path = local_result_path(world, 3, job_id);
    let primary = world.validators.primary_port();
    let finalized_before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality captured before releasing validator-3 Worker");
    let timeout = Instant::now() + Duration::from_secs(600);

    let disposition;
    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("late local OCOMP result must not shut down validator-3");
        let (pid, status) = world
            .localnet
            .owned_validator_process(3)
            .expect("observe exact validator-3 incarnation");
        assert_eq!(
            pid, observation.node_pid,
            "validator-3 must not restart to hide failure"
        );
        assert!(status.is_none());
        world
            .ocomp
            .ensure_worker_alive(3, 0)
            .expect("late worker remains alive");
        assert_eq!(late_validator_worker_pid(world), observation.worker_pid);
        let log = observation
            .node_log
            .read()
            .expect("read exact post-release node log");
        let discarded = log.lines().any(|line| {
            line.contains("ignored late OCOMP result before local persistence")
                && line
                    .split_ascii_whitespace()
                    .any(|field| field == format!("job_id={job_id}"))
                && line
                    .split_ascii_whitespace()
                    .any(|field| field == "reason=\"checkpoint_pruned\"")
        });
        let persisted = match std::fs::read(&result_path) {
            Ok(bytes) => {
                assert_eq!(
                    bytes, observation.canonical_result,
                    "late local result is not canonical"
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("cannot observe late local result: {error}"),
        };
        if let Some(bytes) = read_pending_late_result(&cas, &reference) {
            assert_eq!(
                bytes, observation.canonical_result,
                "late compute output differs from quorum"
            );
            if persisted || discarded {
                disposition = if persisted {
                    "persisted"
                } else {
                    "checkpoint_pruned"
                };
                break;
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-3 did not produce and safely handle the exact late result"
        );
        sleep(Duration::from_millis(250));
    }

    // Require progress after observing the late disposition, not progress that
    // happened while the worker was still stopped.
    let target = world
        .rpc
        .finalized_result(primary)
        .expect("read finality after the late result disposition")
        .max(finalized_before)
        .checked_add(3)
        .expect("post-result finality target");
    let checkpoint = wait_for_common_finalized_checkpoint(world, target, "late validator result");
    let baseline_accountability = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("pre-release canonical votes");
    for port in world.validators.committee_ports() {
        let before = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, finalized_before)
            .expect("read completed job at the pre-release finalized block");
        let after = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, checkpoint.height)
            .expect("read completed job at the post-result common finalized block");
        assert_eq!(before.status, OcompJobStatus::Completed);
        assert_eq!(
            after, before,
            "late compute changed the canonical completed job"
        );
        let accountability = world
            .rpc
            .ocomp_vote_accountability_at_on(port, job_id, checkpoint.height)
            .expect("read post-result votes at the common finalized block");
        assert!(completed_accountability_is_preserved(
            baseline_accountability,
            &accountability
        ));
        assert_eq!(
            accountability.slot_validator_indexes,
            baseline_accountability.slot_validator_indexes
        );
        assert_eq!(
            accountability.slot_first_signatures,
            baseline_accountability.slot_first_signatures
        );
        if checkpoint.height >= request.deadline_height {
            dynamic_deadline_validate_accountability(
                &accountability,
                baseline_accountability,
                &[0, 1, 2],
                request.deadline_height,
                (
                    baseline_accountability.member_count,
                    baseline_accountability.quorum_threshold,
                ),
            )
            .expect("natural deadline closure preserves exactly the original votes");
            assert_eq!(accountability.timely_bitmap, Some(vec![0x07]));
            assert_eq!(accountability.matching_bitmap, Some(vec![0x07]));
            assert_eq!(accountability.missing_bitmap, Some(vec![0x08]));
            assert_eq!(accountability.divergent_bitmap, Some(vec![0x00]));
            assert_eq!(accountability.equivocation_bitmap, Some(vec![0x00]));
        } else {
            assert_eq!(&accountability, baseline_accountability);
        }
    }
    world
        .localnet
        .ensure_committee_alive()
        .expect("committee remains live after result handling");
    world
        .ocomp
        .ensure_worker_alive(3, 0)
        .expect("late worker remains live after finalized progress");
    assert_eq!(late_validator_worker_pid(world), observation.worker_pid);
    let (pid, status) = world.localnet.owned_validator_process(3).unwrap();
    assert_eq!(pid, observation.node_pid);
    assert!(status.is_none());
    eprintln!("E2E_OCOMP_LATE_RESULT job_id={job_id} result_digest={} disposition={disposition} finalized_height={}",
        activation.result_digest, checkpoint.height);

    assert!(
        !world
            .localnet
            .log_has(3, "unexpected embedded OCOMP local-result action")
            .expect("read required owned process log"),
        "validator-3 treated the protocol-owned late result as fatal"
    );
    assert!(
        !world
            .localnet
            .log_has(3, "embedded OCOMP requested local node shutdown")
            .expect("read required owned process log"),
        "validator-3 requested shutdown after its late local result"
    );
}

#[when("the keyless FullNode compute roles stop before the job")]
fn stop_keyless_full_node_compute_before_job(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(index)
        .expect("stop only the keyless FullNode compute clients");
}

#[then("the keyless FullNode holds at the exclusive deadline while validators keep finalizing")]
fn keyless_full_node_holds_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized OCOMP JobIntent");
    let expected_barrier = request.deadline_height;
    let committee_target = request.deadline_height.saturating_add(2);
    let primary = world.validators.primary_port();
    let full_node = world.validators.http_port(world.validators.joiner_index());
    let mut previous_committee = world
        .rpc
        .finalized_result(primary)
        .expect("observe committee finality before FullNode deadline barrier");
    let mut progress_deadline =
        Instant::now() + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);

    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validators remain alive while the FullNode waits");
        let committee_finalized = world
            .rpc
            .finalized_result(primary)
            .expect("observe committee finality at FullNode deadline barrier");
        let full_node_finalized = world.rpc.finalized(full_node);
        if committee_finalized >= committee_target {
            assert_eq!(
                full_node_finalized,
                Some(expected_barrier),
                "FullNode must finalize deadline block D and hold before D+1"
            );
            break;
        }
        if let Some(full_node_finalized) = full_node_finalized {
            assert!(
                full_node_finalized <= expected_barrier,
                "unresolved FullNode advanced past deadline D: deadline={expected_barrier}, \
                 full_node={full_node_finalized}"
            );
        }
        assert!(
            !world
                .localnet
                .joiner_full_node_exited(world.validators.joiner_index()),
            "keyless FullNode exited instead of holding its deadline barrier"
        );
        let now = Instant::now();
        match monotonic_progress_decision(
            committee_finalized,
            committee_target,
            previous_committee,
            now,
            progress_deadline,
        ) {
            ProgressWaitDecision::Reached => unreachable!("target handled above"),
            ProgressWaitDecision::Progressed => {
                previous_committee = committee_finalized;
                progress_deadline = now + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
            }
            ProgressWaitDecision::Waiting => {}
            ProgressWaitDecision::Stalled => {
                panic!(
                    "committee finality stalled before proving the FullNode D boundary: \
                     deadline={}, target={committee_target}, committee={committee_finalized}, \
                     full_node={full_node_finalized:?}",
                    request.deadline_height
                );
            }
        }
        sleep(Duration::from_millis(250));
    }
    let job_id = finalized_job_id(world);
    assert!(!full_node_local_result_path(world, job_id).exists());
    assert!(!dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists());
    world.state.ocomp_full_node_deadline_barrier_height = Some(expected_barrier);
}

#[when("the unresolved keyless FullNode restarts with preserved data")]
fn restart_unresolved_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
}

#[then("the restarted keyless FullNode restores the same deadline barrier without voting")]
fn restarted_keyless_full_node_restores_barrier(world: &mut World) {
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    let barrier = world
        .state
        .ocomp_full_node_deadline_barrier_height
        .expect("captured FullNode deadline barrier");
    assert!(
        world.rpc.wait_finalized_at_least(port, barrier, 90),
        "restarted FullNode did not recover the exact unresolved checkpoint"
    );
    sleep(Duration::from_secs(2));
    assert_eq!(world.rpc.finalized(port), Some(barrier));
    assert!(!world.localnet.joiner_full_node_exited(index));
    assert!(!dynamic_vote_submission_path(world, index, finalized_job_id(world)).exists());
}

#[when("the keyless FullNode compute roles restart after the canonical quorum")]
fn restart_keyless_full_node_compute_after_quorum(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .start_keyless_full_node_roles(index)
        .expect("restart keyless FullNode compute clients after canonical quorum");
}

#[then(
    "the keyless FullNode verifies the exact result and resumes finalized catch-up without voting"
)]
fn keyless_full_node_resumes_after_exact_result(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized canonical OCOMP activation");
    let path = full_node_local_result_path(world, activation.job_id);
    let timeout = Instant::now() + Duration::from_secs(600);
    while !path.is_file() {
        assert!(
            Instant::now() < timeout,
            "keyless FullNode did not persist its exact late result"
        );
        sleep(Duration::from_millis(250));
    }
    let result = LysisResultV1::decode_canonical(
        &std::fs::read(&path).expect("read FullNode late result"),
        &poc_schema_limits(),
    )
    .expect("decode FullNode late result");
    assert_eq!(
        result
            .result_digest(&poc_schema_limits())
            .expect("validate FullNode late result"),
        activation.result_digest
    );
    let target = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("committee finalized target after exact result");
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    assert!(world.rpc.wait_finalized_at_least(port, target, 120));
    assert!(!world.localnet.joiner_full_node_exited(index));
    assert!(!dynamic_vote_submission_path(world, index, activation.job_id).exists());
    world.state.ocomp_full_node_resumed_finalized_height = world.rpc.finalized(port);
}

#[then("the keyless FullNode computes its local result before canonical quorum")]
fn keyless_full_node_computes_before_quorum(world: &mut World) {
    let job_id = finalized_job_id(world);
    let path = full_node_local_result_path(world, job_id);
    let timeout = Instant::now() + Duration::from_secs(600);
    while !path.is_file() {
        assert!(
            Instant::now() < timeout,
            "keyless FullNode did not compute before the remaining validator workers resumed"
        );
        sleep(Duration::from_millis(250));
    }
    let bytes = std::fs::read(&path).expect("read FullNode local-first result");
    let result = LysisResultV1::decode_canonical(&bytes, &poc_schema_limits())
        .expect("decode FullNode local-first result");
    let digest = result
        .result_digest(&poc_schema_limits())
        .expect("validate FullNode local-first result");
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized JobIntent");
    let record = world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .expect("non-quorum job remains public");
    assert_eq!(record.status, OcompJobStatus::VotingOpen);
    assert!(record.terminal.is_none());
    assert!(!dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists());
    world.state.ocomp_full_node_local_result_before_restart = Some(bytes);
    world.state.ocomp_full_node_local_first_digest = Some(digest);
}

#[when("the keyless FullNode restarts with that preserved local result")]
fn restart_keyless_full_node_with_local_result(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner index fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(validator_index)
        .expect("stop FullNode compute clients before local-first restart");
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
    world
        .ocomp
        .start_keyless_full_node_roles(validator_index)
        .expect("restart FullNode compute clients after local-first restart");
    let bytes = std::fs::read(full_node_local_result_path(world, finalized_job_id(world)))
        .expect("read preserved local-first result after restart");
    assert_eq!(
        Some(&bytes),
        world
            .state
            .ocomp_full_node_local_result_before_restart
            .as_ref()
    );
}

#[when("the keyless FullNode arms one valid local-result mismatch")]
fn arm_keyless_full_node_mismatch(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .arm_keyless_full_node_result_mismatch(index)
        .expect("arm one test-only valid FullNode result mutation");
}

#[then("only the keyless FullNode shuts down with durable mismatch evidence")]
fn only_keyless_full_node_shuts_down_on_mismatch(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("canonical activation before mismatch shutdown");
    let job_id = activation.job_id;
    let index = world.validators.joiner_index();
    let timeout = Instant::now() + Duration::from_secs(120);
    while !world.localnet.joiner_full_node_exited(index) {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validator committee remains alive during FullNode mismatch");
        assert!(
            Instant::now() < timeout,
            "mismatched FullNode did not request isolated shutdown"
        );
        sleep(Duration::from_millis(250));
    }
    let evidence_root = world
        .ocomp
        .keyless_full_node_fatal_evidence_root(u8::try_from(index).expect("joiner fits u8"))
        .expect("resolve FullNode fatal evidence root");
    let mut evidence = std::fs::read_dir(&evidence_root)
        .expect("read durable FullNode fatal evidence")
        .map(|entry| entry.expect("read fatal evidence entry").path())
        .collect::<Vec<_>>();
    evidence.sort();
    assert_eq!(
        evidence.len(),
        2,
        "mismatch and sticky evidence are both durable"
    );
    let joined = evidence
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("read fatal evidence file"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains(&format!("job_id={job_id}")));
    assert!(joined.contains("local_result_digest="));
    assert!(joined.contains("canonical_result_digest="));
    world.state.ocomp_full_node_mismatch_job_id = Some(job_id);
    world.state.ocomp_full_node_mismatch_evidence_files = evidence
        .iter()
        .map(|path| {
            path.file_name()
                .expect("fatal evidence file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .finalized_result(world.validators.primary_port())
            .expect("capture finalized height before FullNode mismatch restart"),
    );
}

#[when("the mismatched keyless FullNode restarts with preserved data")]
fn restart_mismatched_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(validator_index)
        .expect("stop external FullNode clients before sticky-fatal restart");
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
}

#[then("it fails closed from sticky evidence while validators keep finalizing")]
fn sticky_mismatch_remains_isolated(world: &mut World) {
    let index = world.validators.joiner_index();
    let timeout = Instant::now() + Duration::from_secs(60);
    while !world.localnet.joiner_full_node_exited(index) {
        assert!(
            Instant::now() < timeout,
            "FullNode restart ignored sticky fatal evidence"
        );
        sleep(Duration::from_millis(250));
    }
    assert!(world
        .localnet
        .log_has(index, "embedded OCOMP persisted fatal evidence")
        .expect("read required owned process log"));
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality before FullNode mismatch restart");
    let primary = world.validators.primary_port();
    assert!(world
        .rpc
        .wait_finalized_at_least(primary, before.saturating_add(2), 60));
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain alive after isolated FullNode fatal");
}

#[then("job A opens with four members and quorum three while job B remains scheduled")]
fn job_a_opens_on_the_historical_four_validator_snapshot(world: &mut World) {
    let worldwide_days = world.state.ocomp_dynamic_worldwide_days.clone();
    let processing_times = world.state.ocomp_dynamic_processing_times.clone();
    assert_eq!(worldwide_days.len(), 2, "job A and job B WorldwideDays");
    assert_eq!(
        processing_times.len(),
        2,
        "job A and job B processing times"
    );
    let job_a_wwd = worldwide_days[0];
    let job_b_wwd = worldwide_days[1];
    let job_b_processing_time = processing_times[1];
    let ports = world.validators.committee_ports();
    let activation_height = world
        .state
        .ocomp_activation_height
        .expect("dynamic OCOMP activation height");
    let deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);

    let (request, record) = loop {
        let requests = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_on(port, activation_height)
                    .unwrap_or_else(|error| panic!("observe job A on port {port}: {error:#}"))
            })
            .collect::<Vec<_>>();
        if requests.iter().all(Option::is_some) {
            let request = requests[0].clone().expect("all job A requests are present");
            assert!(
                requests
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&request)),
                "validators expose different finalized job A requests"
            );
            if request.worldwide_day == job_a_wwd {
                let records = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_job_record_on(port, request.intent_id)
                    })
                    .collect::<Vec<_>>();
                if records.iter().all(|record| {
                    record
                        .as_ref()
                        .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
                }) {
                    let record = records[0].clone().expect("all job A records are present");
                    assert!(
                        records
                            .iter()
                            .all(|observed| observed.as_ref() == Some(&record)),
                        "validators expose different canonical job A records"
                    );
                    break (request, record);
                }
            }
        }

        let latest_timestamp = world
            .rpc
            .latest_block_timestamp(world.validators.primary_port())
            .expect("canonical block timestamp while waiting for job A");
        assert!(
            latest_timestamp < job_b_processing_time,
            "job B processing time arrived before job A became publicly observable"
        );
        assert!(
            Instant::now() < deadline,
            "job A did not become VotingOpen through the public finalized path"
        );
        sleep(Duration::from_millis(500));
    };

    assert_eq!(record.intent.wwd, job_a_wwd);
    assert_eq!(record.intent.result_member_count, 4);
    assert_eq!(record.intent.result_quorum_threshold, 3);
    assert_ne!(record.intent.result_committee_set_hash, B256::ZERO);
    assert_ne!(record.intent.result_ocomp_binding_hash, B256::ZERO);
    assert!(
        ports
            .iter()
            .copied()
            .all(|port| world.rpc.active_version_on(port) == Some(0)),
        "a real post-activation OCOMP job must not depend on a generic Update"
    );
    assert!(
        ports.iter().copied().all(|port| world
            .rpc
            .metadosis_wwd_state_on(port, job_b_wwd)
            .is_some_and(|day| {
                day.status == 2 && day.scheduled_process_time == job_b_processing_time
            })),
        "job B must remain in its canonical OFFERING schedule while job A opens"
    );
    assert!(
        world
            .rpc
            .latest_block_timestamp(world.validators.primary_port())
            .is_some_and(|timestamp| timestamp < job_b_processing_time),
        "job B must not reach its processing time during the job A assertion"
    );
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality captured before stopping OCOMP workers");
    let after = world
        .rpc
        .finalized_result(world.validators.primary_port())
        .expect("observe finality with two OCOMP workers stopped");
    assert!(
        after > before,
        "consensus finality did not advance with two OCOMP Workers stopped"
    );
    world.state.ocomp_dynamic_job_requests = vec![request];
}

#[when("validators 1, 2 and 3 OCOMP workers are stopped before the job")]
fn stop_three_workers_before_job(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 0,
            })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Worker before the job: {error}")
            });
    }
}

#[when("one valid vote is finalized and a changed-binding vote is submitted")]
fn one_valid_then_changed_binding_vote(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized single-voter JobIntent");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let (job_id, accountability) = loop {
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
        }) {
            let record = records[0].clone().expect("all records are present");
            assert!(
                records
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&record)),
                "validators expose different single-voter job state"
            );
            let job_id = record
                .finalized
                .as_ref()
                .expect("finalized single-voter job")
                .job_id;
            let accountability = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id);
            if accountability
                .as_ref()
                .is_some_and(|value| value.slot_validator_indexes == [0])
            {
                eprintln!(
                    "OCOMP_PUBLIC_MUTATION stage=single_vote_observed finalized_height={:?}",
                    world.rpc.finalized(primary)
                );
                break (job_id, accountability.expect("checked above"));
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-0 did not finalize the sole public result vote before mutation"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(accountability.quorum_result_digest, None);

    let finalized_height = world.rpc.finalized(primary).expect("finalized height");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes from={} to={finalized_height}",
        request.request_height
    );
    let public_votes = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            primary,
            request.request_height,
            finalized_height,
        )
        .expect("enumerate the sole finalized public vote");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes_done observed={}",
        public_votes.len()
    );
    let successful = public_votes
        .iter()
        .filter(|transaction| transaction.success)
        .collect::<Vec<_>>();
    assert_eq!(successful.len(), 1, "expected one successful public vote");
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, successful[0].transaction_hash)
        .expect("decode sole public ResultVoteV1");
    let mut mutated = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical sole ResultVoteV1");
    assert_eq!(mutated.job_id, job_id);
    let delegate_key = world
        .ocomp
        .ocomp_delegate_private_key_for_vote(&mutated)
        .expect("sole vote OCOMP delegate key");
    mutated.job_id = B256::repeat_byte(0x5a);
    eprintln!("OCOMP_PUBLIC_MUTATION stage=submit_changed_binding");
    let mutation = world.rpc.submit_ocomp_result_vote_bytes(
        primary,
        &delegate_key,
        mutated
            .encode_canonical(&poc_schema_limits())
            .expect("canonical changed-binding payload"),
    );
    let mutation_height = match mutation {
        Ok(mutation_hash) => {
            eprintln!("OCOMP_PUBLIC_MUTATION stage=changed_binding_receipt tx={mutation_hash}");
            let mutation_receipt = world
                .rpc
                .transaction_receipt(&mutation_hash, primary)
                .expect("changed-binding vote receipt");
            assert_eq!(
                mutation_receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str),
                Some("0x0"),
                "changed-binding vote must revert"
            );
            Some(
                world
                    .rpc
                    .receipt_block_number(&mutation_hash, primary)
                    .expect("changed-binding inclusion height"),
            )
        }
        Err(error) => {
            assert!(
                error
                    .to_string()
                    .contains("OCOMP carrier signer is not authorized for this action"),
                "changed-binding vote failed for an unexpected reason: {error:#}"
            );
            None
        }
    };
    world.state.ocomp_non_quorum_changed_binding_reverted = Some(true);
    if let Some(mutation_height) = mutation_height {
        eprintln!(
            "OCOMP_PUBLIC_MUTATION stage=wait_changed_binding_finality inclusion_height={mutation_height}"
        );
        assert!(
            world
                .rpc
                .wait_finalized_at_least(primary, mutation_height, 60),
            "changed-binding vote did not finalize"
        );
    }
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=changed_binding_finalized finalized_height={:?}",
        world.rpc.finalized(primary)
    );

    for port in ports {
        let after = world
            .rpc
            .finalized_ocomp_job_record_on(port, request.intent_id)
            .expect("job after changed-binding vote");
        assert_eq!(after.status, OcompJobStatus::VotingOpen);
        assert!(after.terminal.is_none());
        assert_eq!(
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(port, job_id)
                .expect("accountability after changed-binding vote"),
            accountability
        );
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false)
        );
    }
    world.state.ocomp_non_quorum_state_unchanged = Some(true);
}

#[when("the three stopped workers restart and form the remaining quorum")]
fn restart_three_workers_for_quorum(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker for quorum: {error}")
            });
    }
}

#[then("the no-quorum job expires at its exclusive deadline without creating Nod")]
fn no_quorum_job_expires_without_nod(world: &mut World) {
    assert_job_expires_without_nod(world, &[0, 1], 0b0011, 0b1100, true, "two-vote");
}

#[then("the unexported zero-vote job expires and reaches Released without an ACK")]
fn unexported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(world, &[], 0, 0b1111, false, "zero-vote unexported");
}

#[then("the exported zero-vote job expires at its exclusive deadline and finality continues")]
fn exported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(world, &[], 0, 0b1111, true, "zero-vote exported");
    world
        .ocomp
        .ensure_worker_cohort_stopped(
            world
                .state
                .ocomp_worker_outage
                .as_ref()
                .expect("owned worker outage evidence"),
        )
        .expect("the same worker cohort remains stopped through expiry and retention release");
}

fn assert_job_expires_without_nod(
    world: &mut World,
    expected_voters: &[u16],
    timely_bitmap: u8,
    missing_bitmap: u8,
    expect_export: bool,
    fault_label: &str,
) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized no-quorum JobIntent");
    let primary = world.validators.primary_port();
    wait_for_common_finalized_checkpoint(world, request.deadline_height, fault_label);
    let records = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, request.deadline_height)
                .unwrap_or_else(|error| {
                    panic!(
                        "read {fault_label} OCOMP record at exact deadline h{} on port {port}: {error:#}",
                        request.deadline_height
                    )
                })
        })
        .collect::<Vec<_>>();
    let record = records[0].clone();
    assert!(
        records.iter().all(|observed| observed == &record),
        "validators expose different expired JobIntent state"
    );
    assert_eq!(
        record.status,
        OcompJobStatus::Expired,
        "{fault_label} OCOMP job was not expired at its exact deadline"
    );
    let terminal = record.terminal.expect("expired terminal record");
    assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
    assert_eq!(terminal.terminal_height, request.deadline_height);
    assert!(terminal.completed_binding.is_none());
    let finalized = record.finalized.expect("finalized expired job");
    assert!(finalized.quorum.is_none());

    let accountability = world
        .rpc
        .ocomp_vote_accountability_at_on(primary, finalized.job_id, request.deadline_height)
        .expect("closed no-quorum accountability");
    assert_eq!(accountability.slot_validator_indexes, expected_voters);
    assert_eq!(accountability.quorum_result_digest, None);
    assert_eq!(accountability.closed_height, Some(request.deadline_height));
    assert_eq!(accountability.timely_bitmap, Some(vec![timely_bitmap]));
    assert_eq!(accountability.missing_bitmap, Some(vec![missing_bitmap]));
    assert_eq!(accountability.equivocation_bitmap, Some(vec![0]));

    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .ocomp_vote_accountability_at_on(port, finalized.job_id, request.deadline_height)
                .expect("exact closed accountability on every validator"),
            accountability
        );
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false),
            "expired no-quorum job created a Nod generation on port {port}"
        );
        assert!(
            world
                .rpc
                .finalized_ocomp_activation_absent_on(
                    port,
                    request.request_height,
                    request.intent_id,
                )
                .expect("prove absence of finalized Lysis activation"),
            "expired no-quorum job emitted a Lysis apply event on port {port}"
        );
    }
    let retention_blocks = world
        .ocomp
        .canonical_fork_install()
        .expect("read canonical OCOMP retention profile")
        .request_profile
        .capacity_profile
        .source_retention_after_terminal_blocks;
    let terminal_checkpoint = request
        .deadline_height
        .saturating_add(retention_blocks)
        .saturating_add(1);
    wait_for_common_finalized_checkpoint(world, terminal_checkpoint, fault_label);
    for port in world.validators.committee_ports() {
        let record = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, terminal_checkpoint)
            .expect("expired record after terminal finalized checkpoint");
        assert_eq!(record.status, OcompJobStatus::Expired);
        assert_eq!(
            record
                .terminal
                .as_ref()
                .map(|terminal| terminal.terminal_height),
            Some(request.deadline_height)
        );
    }
    assert!(
        terminal_checkpoint
            > world
                .state
                .ocomp_finality_before_fault
                .expect("finality captured before no-quorum OCOMP fault"),
        "terminal checkpoint must advance beyond pre-fault finality"
    );
    wait_for_released_retention(world, finalized.job_id, expect_export, fault_label);
    world.state.ocomp_vote_accountability = Some(accountability);
    world.state.ocomp_expired_without_nod = Some(true);
}

fn wait_for_released_retention(
    world: &mut World,
    job_id: B256,
    expect_export: bool,
    fault_label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut latest = vec![String::new(); 4];
    loop {
        let mut released = 0_usize;
        for (validator_index, observation) in latest.iter_mut().enumerate() {
            let root = retention_journal_root(&world.validators.data_dir(validator_index));
            match inspect_retention_journal(&root) {
                Ok(snapshot) => {
                    let matching = snapshot.records.iter().find_map(|(_, record)| {
                        let PinStateV1::Released {
                            job_id: Some(observed_job_id),
                            source_generation,
                            reason,
                            observed_height,
                            export,
                            ..
                        } = record.state
                        else {
                            return None;
                        };
                        (observed_job_id == job_id).then_some((
                            source_generation,
                            reason,
                            observed_height,
                            export,
                        ))
                    });
                    if let Some((source_generation, reason, observed_height, export)) = matching {
                        assert_eq!(
                            reason,
                            PinReleaseReason::RetentionSatisfied,
                            "validator-{validator_index} released {fault_label} job for the wrong reason"
                        );
                        assert!(
                            source_generation.is_some(),
                            "validator-{validator_index} lost the released source generation"
                        );
                        assert_eq!(
                            export.is_some(),
                            expect_export,
                            "validator-{validator_index} released {fault_label} job with unexpected export authority"
                        );
                        if let Some(outage) = &world.state.ocomp_worker_outage {
                            let saved = outage
                                .exports
                                .iter()
                                .find(|item| item.validator_index as usize == validator_index)
                                .expect("saved exact export for each faulted validator");
                            assert_eq!(saved.job_id, job_id);
                            assert_eq!(source_generation, Some(saved.source_generation));
                            assert_eq!(
                                export,
                                Some(outbe_node::ocomp::retention::ExportAuthorityV1 {
                                    source_generation: saved.source_generation,
                                    lease_generation: saved.lease_generation,
                                    manifest_hash: saved.manifest_hash,
                                })
                            );
                            let retention = world
                                .ocomp
                                .canonical_fork_install()
                                .unwrap()
                                .request_profile
                                .capacity_profile
                                .source_retention_after_terminal_blocks;
                            let due = world
                                .state
                                .ocomp_job_request
                                .as_ref()
                                .unwrap()
                                .deadline_height
                                .checked_add(retention)
                                .unwrap();
                            assert!(
                                observed_height >= due,
                                "retention released before its canonical due height"
                            );
                        }
                        *observation = format!(
                            "Released(reason={reason:?}, observed_height={observed_height}, export={})",
                            export.is_some()
                        );
                        released += 1;
                    } else {
                        *observation = format!(
                            "journal_generation={}, records={}, matching_job=absent",
                            snapshot.generation,
                            snapshot.records.len()
                        );
                    }
                }
                Err(error) => {
                    *observation = format!("journal error at {}: {error}", root.display());
                }
            }
        }
        if released == 4 {
            break;
        }
        world
            .localnet
            .ensure_committee_alive()
            .expect("consensus committee remains live while retention GC completes");
        assert!(
            Instant::now() < deadline,
            "{fault_label} job {job_id:#x} did not reach durable Released on every validator: {latest:?}"
        );
        sleep(Duration::from_millis(250));
    }
    world.state.ocomp_expired_release_had_export = Some(expect_export);
}

fn retention_journal_root(node_data_dir: &Path) -> PathBuf {
    node_data_dir.join("consensus").join("ocomp_retention")
}

#[when("the stopped OCOMP workers restart after canonical expiry")]
fn stopped_workers_restart_after_expiry(world: &mut World) {
    for validator_index in [2, 3] {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker after canonical expiry: {error}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP roles are live after canonical expiry");
}

#[when("all stopped OCOMP snapshot exporters restart after canonical expiry")]
fn all_exporters_restart_after_expiry(world: &mut World) {
    for validator_index in 0..4 {
        world
            .ocomp
            .restart_snapshot_exporter(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} SnapshotExporter: {error:#}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP roles are live after canonical expiry");
}

#[when("all stopped OCOMP workers restart after canonical expiry")]
fn all_workers_restart_after_expiry(world: &mut World) {
    world
        .ocomp
        .ensure_worker_cohort_stopped(
            world
                .state
                .ocomp_worker_outage
                .as_ref()
                .expect("owned worker outage evidence"),
        )
        .expect("no worker incarnation restarted before the explicit recovery step");
    for validator_index in 0..4 {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker: {error:#}")
            });
    }
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all four workers reconnect to their embedded supervisors after expiry");
}

#[then("the expired job remains terminal with no successor after process recovery")]
fn expired_job_remains_terminal_without_successor(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired public JobIntent");
    let ports = world.validators.committee_ports();
    let current = ports
        .iter()
        .copied()
        .map(|port| {
            world
                .rpc
                .finalized_result(port)
                .expect("validator finality after recovery")
        })
        .collect::<Vec<_>>();
    let target = current
        .into_iter()
        .max()
        .expect("four validator finality observations")
        .saturating_add(2);
    wait_for_common_finalized_checkpoint(world, target, "post-expiry process recovery");

    let mut expired_job_id = None;
    for port in ports {
        let retained = world
            .rpc
            .ocomp_job_record_at_on(port, original.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read expired job at post-recovery h{target} on port {port}: {error:#}")
            });
        assert_eq!(retained.status, OcompJobStatus::Expired);
        assert_eq!(
            retained
                .terminal
                .as_ref()
                .expect("expired terminal remains present")
                .outcome,
            OcompTerminalOutcome::Expired
        );
        let observed_job_id = retained
            .finalized
            .as_ref()
            .expect("expired record keeps finalized job identity")
            .job_id;
        if let Some(expected_job_id) = expired_job_id {
            assert_eq!(
                observed_job_id, expected_job_id,
                "validators disagree on the expired canonical job id"
            );
        } else {
            expired_job_id = Some(observed_job_id);
        }
        let successor = world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(
                port,
                original.request_height.saturating_add(1),
                original.worldwide_day,
            )
            .unwrap_or_else(|error| {
                panic!("prove no successor request on RPC port {port}: {error:#}")
            });
        assert!(
            successor.is_absent(),
            "expired job created a forbidden successor for the same WWD on port {port}: {successor:?}"
        );
    }
    wait_for_released_retention(
        world,
        expired_job_id.expect("expired finalized job id after recovery"),
        world
            .state
            .ocomp_expired_release_had_export
            .expect("released export authority observation"),
        "post-expiry process recovery",
    );
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain live after terminal recovery");
}

#[when("one independent next-day Tribute is submitted after OCOMP recovery")]
fn submit_independent_next_day_tribute_after_recovery(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired public JobIntent");
    let original_wwd = WorldwideDay::new(original.worldwide_day);
    let followup_wwd = WorldwideDay::from_timestamp(
        original_wwd
            .start_timestamp()
            .checked_add(86_400)
            .expect("next WorldwideDay timestamp"),
    )
    .value();
    assert_ne!(followup_wwd, original.worldwide_day);

    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let states = ports
        .iter()
        .copied()
        .map(|port| world.rpc.metadosis_wwd_state_on(port, followup_wwd))
        .collect::<Vec<_>>();
    let schedule = states[0]
        .clone()
        .expect("recovery fixture exposes its independent next-day WorldwideDay");
    assert_eq!(
        schedule.status, 2,
        "independent next-day WorldwideDay must be exactly OFFERING before submission: {schedule:?}"
    );
    assert!(
        states
            .iter()
            .all(|candidate| candidate.as_ref() == Some(&schedule)),
        "validators disagree on the next-day OFFERING state: {states:?}"
    );
    let finalized_height = world
        .rpc
        .finalized_result(primary)
        .expect("read finalized height before the independent Tribute");
    let finalized_timestamp = world
        .rpc
        .block_timestamp(primary, finalized_height)
        .expect("read finalized timestamp before the independent Tribute");
    assert!(
        finalized_timestamp < schedule.offering_end,
        "independent next-day offering already closed before submission: finalized_timestamp={finalized_timestamp}, schedule={schedule:?}"
    );
    for port in &ports {
        let existing = world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(*port, 0, followup_wwd)
            .unwrap_or_else(|error| {
                panic!("prove no pre-seeded next-day JobIntent on RPC port {port}: {error:#}")
            });
        assert!(
            existing.is_absent(),
            "recovery fixture pre-seeded a forbidden next-day JobIntent on RPC port {port}: {existing:?}"
        );
    }

    let offerer = world
        .validators
        .by_name("validator-0")
        .expect("validator-0 next-day Tribute owner")
        .evm_key()
        .expect("validator-0 next-day Tribute key");
    let tribute_tx = world
        .rpc
        .tribute_offer(&offerer, &followup_wwd.to_string())
        .expect("submit independent next-day Tribute");
    assert!(
        world.rpc.wait_successful_receipt(&tribute_tx, 240),
        "independent next-day Tribute transaction failed: {tribute_tx}"
    );
    world
        .projection
        .wait_for_tribute_projection(&tribute_tx, 240)
        .expect("all exporters project the independent next-day Tribute");
    world.state.tribute_tx_hash = Some(tribute_tx);

    let processing_target =
        first_protocol_cycle_at_or_after(world, schedule.scheduled_process_time);
    let _ = restart_committee_at_logical_time(world, processing_target);
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("OCOMP roles survive the next-day processing-time restart");

    let request_deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let request = loop {
        let observed = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_for_worldwide_day_result_on(
                        port,
                        original.deadline_height.saturating_add(1),
                        followup_wwd,
                    )
                    .and_then(|observation| observation.into_bound_request())
                    .unwrap_or_else(|error| {
                        panic!(
                            "observe independent next-day OCOMP request on port {port}: {error:#}"
                        )
                    })
            })
            .collect::<Vec<_>>();
        if observed.iter().all(Option::is_some) {
            let first = observed[0]
                .clone()
                .expect("all independent next-day requests are present");
            assert!(
                observed
                    .iter()
                    .all(|candidate| candidate.as_ref() == Some(&first)),
                "validators expose different independent next-day requests: {observed:?}"
            );
            break first;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles remain live while waiting for the next-day request");
        assert!(
            Instant::now() < request_deadline,
            "independent next-day OCOMP request was not finalized: {observed:?}"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(request.worldwide_day, followup_wwd);
    assert_eq!(request.pending_nonce, 0);
    assert_eq!(request.attempt, 0);
    assert_ne!(request.intent_id, original.intent_id);
    assert!(request.request_height > original.deadline_height);
    world.state.ocomp_followup_job_request = Some(request);
}

#[then("the independent OCOMP job completes on every validator")]
fn independent_ocomp_job_completes_on_every_validator(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired original JobIntent");
    let followup = world
        .state
        .ocomp_followup_job_request
        .clone()
        .expect("independent next-day JobIntent");
    quorum_applies_lysis_and_creates_nod_for_request(
        world,
        followup.clone(),
        PublicVoteSetExpectation::Exact(&[0, 1, 2, 3]),
    );
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("independent next-day Lysis activation");
    let target = activation.block_number.saturating_add(2);
    wait_for_common_finalized_checkpoint(world, target, "independent next-day completion");
    for port in world.validators.committee_ports() {
        let completed = world
            .rpc
            .ocomp_job_record_at_on(port, followup.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read completed independent job at h{target} on port {port}: {error:#}")
            });
        assert_eq!(completed.status, OcompJobStatus::Completed);
        let expired = world
            .rpc
            .ocomp_job_record_at_on(port, original.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read original expired job at h{target} on port {port}: {error:#}")
            });
        assert_eq!(expired.status, OcompJobStatus::Expired);
    }
    world.state.ocomp_followup_completed_finality = Some(target);
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain live after the independent job completes");
}

#[then("all four OCOMP domains run their node-facing production roles")]
fn four_domains_run_node_facing_roles(world: &mut World) {
    let counts = world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("Node-owned OCOMP endpoints, exporters and workers are ready");
    assert_eq!(counts.supervisors, 4);
    assert_eq!(counts.snapshot_exporters, 4);
    let records = world.ocomp.process_records();
    for validator_index in 0..4_u8 {
        let role = OcompProcessRole::SnapshotExporter;
        let matches = records
            .iter()
            .filter(|record| {
                record.validator_index == Some(validator_index)
                    && record.role == role
                    && record.worker_ordinal.is_none()
                    && record.stopped_at_millis.is_none()
            })
            .count();
        assert_eq!(
            matches, 1,
            "validator-{validator_index} must own one live {role:?}"
        );
    }
}

#[then("all four OCOMP domains use the production basedir contract")]
fn four_domains_use_production_basedir(world: &mut World) {
    world
        .ocomp
        .verify_release_basedir_contract()
        .expect("all release OCOMP roles use the scenario basedir contract");
}

#[when("validator 0 SnapshotExporter restarts with its committed export intact")]
fn snapshot_exporter_preserves_committed_export(world: &mut World) {
    let job_id = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("completed activation before committed exporter restart")
        .job_id;
    world
        .ocomp
        .verify_committed_exporter_restart(0, job_id)
        .expect("SnapshotExporter preserves its exact acknowledged export across restart");
}

#[then("each OCOMP domain owns one authenticated production worker")]
fn four_domains_own_authenticated_workers(world: &mut World) {
    let records = world.ocomp.process_records();
    assert_eq!(
        records.len(),
        8,
        "expected one external RPC exporter and one worker for each Node-owned Supervisor"
    );
    for validator_index in 0..4_u8 {
        let workers = records
            .iter()
            .filter(|record| {
                record.validator_index == Some(validator_index)
                    && record.role == OcompProcessRole::Worker
                    && record.worker_ordinal == Some(0)
                    && record.stopped_at_millis.is_none()
            })
            .count();
        assert_eq!(
            workers, 1,
            "validator-{validator_index} must own one live authenticated worker"
        );
    }
}

#[then("each OCOMP domain retains isolated deterministic worker artifacts for that JobIntent")]
fn four_domains_retain_isolated_worker_artifacts(world: &mut World) {
    let intent_id = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("V1 JobIntent")
        .intent_id;
    let bundle_hash = world
        .ocomp
        .canonical_fork_install()
        .expect("V1 fork authority")
        .request_profile
        .protocol_bundle_hash;
    verify_case_one_completed_artifacts(world, intent_id, bundle_hash)
        .expect("V1 job has four exact computations and canonical-voter submission evidence");
}

/// Parent orchestration calls this for V1 immediately before releasing held
/// V1 workers, and for V2 after its last node restart but BEFORE V2 work is
/// released. Do not insert a post-hoc call after the processing-time jump.
pub(crate) fn arm_case_one_artifact_phase(
    world: &mut World,
    bundle_hash: B256,
    budget: Duration,
) -> eyre::Result<()> {
    world.localnet.ensure_committee_alive()?;
    ensure!(
        world.validators.size() == 4,
        "artifact phase requires the four founders"
    );
    let pids = (0..4_u8)
        .map(|index| {
            world
                .localnet
                .validator_pid(usize::from(index))
                .map(|pid| (index, pid))
        })
        .collect::<eyre::Result<std::collections::BTreeMap<_, _>>>()?;
    world
        .ocomp
        .arm_completed_artifact_phase(bundle_hash, pids, budget)
}

fn verify_case_one_completed_artifacts(
    world: &mut World,
    intent_id: B256,
    bundle_hash: B256,
) -> eyre::Result<()> {
    world.localnet.ensure_committee_alive()?;
    let ports = world.validators.committee_ports();
    ensure!(
        ports.len() == 4,
        "artifact proof omitted an expected validator"
    );
    let height = ports
        .iter()
        .map(|&port| world.rpc.finalized_result(port))
        .collect::<eyre::Result<Vec<_>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| eyre!("artifact proof has no finalized checkpoint"))?;
    let checkpoint = world.rpc.checkpoint_at(ports[0], height)?;
    let record = world
        .rpc
        .ocomp_job_record_at_on(ports[0], intent_id, height)?;
    ensure!(
        record.status == OcompJobStatus::Completed,
        "artifact job is not Completed"
    );
    ensure!(
        record.intent.protocol_bundle_hash == bundle_hash,
        "artifact job changed bundle"
    );
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| eyre!("artifact job lacks finality"))?;
    let job_id = finalized.job_id;
    let binding = record
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.completed_binding.as_ref())
        .ok_or_else(|| eyre!("artifact job lacks canonical completed binding"))?;
    let accountability = world
        .rpc
        .ocomp_vote_accountability_at_on(ports[0], job_id, height)?;
    ensure!(
        accountability.job_id == job_id
            && accountability.member_count == 4
            && accountability.quorum_threshold == 3
            && accountability.quorum_height == Some(binding.quorum_height)
            && accountability.quorum_result_digest == Some(binding.result_digest)
            && accountability
                .quorum_signer_bitmap
                .as_ref()
                .is_some_and(|bitmap| {
                    bitmap.iter().map(|byte| byte.count_ones()).sum::<u32>() == 3
                })
            && binding.quorum_height <= height,
        "artifact quorum differs from canonical completed authority"
    );
    for &port in &ports {
        ensure!(
            world.rpc.checkpoint_at(port, height)? == checkpoint,
            "artifact checkpoint differs"
        );
        ensure!(
            world.rpc.ocomp_job_record_at_on(port, intent_id, height)? == record,
            "artifact job differs at shared finalized checkpoint"
        );
        ensure!(
            world
                .rpc
                .ocomp_vote_accountability_at_on(port, job_id, height)?
                == accountability,
            "artifact voters differ at shared finalized checkpoint"
        );
    }
    let quorum_checkpoint = world.rpc.checkpoint_at(ports[0], binding.quorum_height)?;
    let transactions = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            ports[0],
            binding.quorum_height,
            binding.quorum_height,
        )
        .ok_or_else(|| eyre!("cannot enumerate canonical quorum transactions"))?;
    let mut canonical_result = None;
    for transaction in transactions
        .iter()
        .filter(|transaction| transaction.success)
    {
        ensure!(
            transaction.block_number == binding.quorum_height
                && transaction.block_hash == quorum_checkpoint.block_hash,
            "quorum transaction has a foreign canonical block"
        );
        let bytes = world
            .rpc
            .ocomp_result_vote_bytes_on(ports[0], transaction.transaction_hash)
            .ok_or_else(|| eyre!("cannot read canonical quorum vote"))?;
        let vote = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits())?;
        if vote.job_id != job_id {
            continue;
        }
        ensure!(
            vote.protocol_bundle_hash == bundle_hash
                && vote.result_validator_set_epoch == accountability.result_validator_set_epoch
                && vote.result_committee_set_hash == accountability.result_committee_set_hash
                && vote.result_ocomp_binding_hash == accountability.result_ocomp_binding_hash,
            "quorum vote has a foreign job binding"
        );
        if vote.result.result_digest(&poc_schema_limits())? != binding.result_digest {
            continue;
        }
        if let Some(previous) = &canonical_result {
            ensure!(previous == &vote.result, "canonical quorum results differ");
        }
        canonical_result = Some(vote.result);
    }
    let result =
        canonical_result.ok_or_else(|| eyre!("canonical completed result is unavailable"))?;
    ensure!(
        result.job_id == job_id,
        "canonical result belongs to another job"
    );
    let proof = crate::world::ocomp::OcompCanonicalArtifactProof {
        checkpoint,
        bundle_hash,
        result,
        voters: accountability.slot_validator_indexes,
    };
    loop {
        world.localnet.ensure_committee_alive()?;
        let pids = (0..4_u8)
            .map(|index| {
                world
                    .localnet
                    .validator_pid(usize::from(index))
                    .map(|pid| (index, pid))
            })
            .collect::<eyre::Result<std::collections::BTreeMap<_, _>>>()?;
        if let Some(evidence) = world
            .ocomp
            .verify_completed_artifacts_canonical(&proof, &pids)?
        {
            world.localnet.ensure_committee_alive()?;
            eprintln!("OCOMP_ARTIFACT_PROOF_V1 {evidence}");
            return Ok(());
        }
        sleep(Duration::from_millis(250));
    }
}

#[when("validator 0 OCOMP worker is stopped through the typed fault control")]
fn stop_validator_zero_worker(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .finalized_result(primary)
            .expect("capture finalized height before typed OCOMP worker fault"),
    );
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopWorker {
            validator_index: 0,
            worker_ordinal: 0,
        })
        .expect("stop only validator-0 worker");
}

#[then("consensus finality advances while only that worker remains stopped")]
fn finality_advances_after_worker_stop(world: &mut World) {
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("height captured before OCOMP fault");
    let primary = world.validators.primary_port();
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, before.saturating_add(2), 60),
        "consensus finality did not advance after stopping an OCOMP worker"
    );
    let after = world.rpc.finalized(primary).expect("finalized height");
    assert!(after >= before.saturating_add(2));

    let records = world.ocomp.process_records();
    let stopped = records
        .iter()
        .filter(|record| record.stopped_at_millis.is_some())
        .collect::<Vec<_>>();
    assert_eq!(
        stopped.len(),
        1,
        "fault must stop exactly one owned process"
    );
    assert_eq!(stopped[0].validator_index, Some(0));
    assert_eq!(stopped[0].role, OcompProcessRole::Worker);
    assert_eq!(stopped[0].worker_ordinal, Some(0));
}

#[then("validator 0 OCOMP worker restarts through the typed topology")]
fn validator_zero_worker_restarts(world: &mut World) {
    world
        .ocomp
        .restart_worker(0, 0)
        .expect("restart only validator-0 OCOMP worker");
    let records = world.ocomp.process_records();
    let validator_zero_workers = records
        .iter()
        .filter(|record| {
            record.validator_index == Some(0)
                && record.role == OcompProcessRole::Worker
                && record.worker_ordinal == Some(0)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validator_zero_workers.len(),
        2,
        "restart must retain the stopped lifecycle record and add one process"
    );
    assert_eq!(
        validator_zero_workers
            .iter()
            .filter(|record| record.stopped_at_millis.is_none())
            .count(),
        1,
        "exactly one validator-0 worker must be live after restart"
    );
}

#[when("all validator nodes and OCOMP node-facing processes restart with preserved data")]
fn restart_completed_network_and_ocomp_processes(world: &mut World) {
    let primary = world.validators.primary_port();
    let before = world
        .rpc
        .finalized(primary)
        .expect("finality before restart");

    // External clients depend on node RPC and projection storage. Stop the
    // complete cohort before taking down any validator, exactly as the initial
    // production-shaped launch starts them only after committee readiness.
    let ocomp_resume = stop_ocomp_roles_before_committee_time_change(world);
    world
        .localnet
        .restart_committee_preserving_enclaves()
        .unwrap_or_else(|error| {
            panic!("restart complete committee with preserved datadirs and enclaves: {error:#}")
        });
    for validator_index in 0..4 {
        let port = world.validators.http_port(validator_index);
        assert!(
            world.rpc.wait_block(port, before, 60).is_some(),
            "validator-{validator_index} did not restore its preserved finalized head"
        );
    }

    // A shared historical floor is not enough: require one fresh canonical
    // finalization after the complete cohort returns from the quiescent barrier.
    let convergence_target = post_restart_convergence_target(
        world.validators.committee_ports().into_iter().map(|port| {
            world
                .rpc
                .finalized(port)
                .expect("finalized height after validator restart")
        }),
    );
    let _ = finalized_points_at_common_height(world, convergence_target);
    restart_ocomp_roles_after_committee_time_change(world, ocomp_resume);
}

#[then("the completed generation and exact vote replay remain identical")]
fn completed_generation_survives_restart_and_replay(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("completed JobIntent before restart");
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("completed activation before restart");
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("certified generation before restart");

    for port in world.validators.committee_ports() {
        assert!(
            world
                .rpc
                .wait_finalized_at_least(port, activation.block_number, 60),
            "validator on port {port} did not recover the activation height"
        );
        let recovered_activation = world
            .rpc
            .finalized_ocomp_activation_on(port, request.request_height, request.intent_id)
            .expect("recovered finalized activation");
        assert_eq!(
            recovered_activation, activation,
            "restart changed finalized activation on port {port}"
        );
        let recovered_generation = world
            .rpc
            .finalized_ocomp_certified_generation_on(port, &recovered_activation)
            .expect("recovered certified generation");
        assert_eq!(
            recovered_generation, generation,
            "restart changed certified generation on port {port}"
        );
        assert!(
            world
                .rpc
                .transaction_receipt(&format!("{:#x}", activation.transaction_hash), port)
                .is_some(),
            "restart lost the q-forming transaction receipt on port {port}"
        );
    }

    let primary = world.validators.primary_port();
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, activation.transaction_hash)
        .expect("decode the original q-forming result vote after restart");
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical q-forming ResultVoteV1 after restart");
    let delegate_key = world
        .ocomp
        .ocomp_delegate_private_key_for_vote(&vote)
        .expect("q-forming vote OCOMP delegate key after restart");
    let replay_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &delegate_key, vote_bytes)
        .expect("submit exact full-result replay after restart");
    let replay_receipt = world
        .rpc
        .transaction_receipt(&replay_hash, primary)
        .expect("exact post-restart replay receipt");
    assert_eq!(
        replay_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x1"),
        "exact full-result replay after restart was not idempotently accepted"
    );
    let after = world
        .rpc
        .finalized_ocomp_activation_on(primary, request.request_height, request.intent_id)
        .expect("activation after exact replay");
    assert_eq!(after, activation, "exact replay changed the activation");
    let after_generation = world
        .rpc
        .finalized_ocomp_certified_generation_on(primary, &after)
        .expect("generation after exact replay");
    assert_eq!(
        after_generation, generation,
        "exact replay changed the certified generation"
    );
    world.state.ocomp_restart_replay_verified = Some(true);
}

#[when("a late follower replays the finalized OCOMP request and quorum blocks")]
fn late_follower_replays_ocomp_history(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized activation before historical replay");
    world
        .ocomp
        .stage_cold_history_follower_bundles(OCOMP_TRACE_FOLLOWER_SLOT)
        .expect(
            "install only validated public bundles before cold historical follower provisioning",
        );
    world
        .localnet
        .provision_full_node_node_host(OCOMP_TRACE_FOLLOWER_SLOT)
        .expect("provision late historical-replay FullNode NodeHost");
    world
        .localnet
        .launch_dcap_full_node("follower", OCOMP_TRACE_FOLLOWER_SLOT, 0)
        .expect("launch late historical-replay follower");
    let follower_port = world.validators.http_port(OCOMP_TRACE_FOLLOWER_SLOT);
    assert!(
        world
            .rpc
            .wait_finalized_at_least(follower_port, activation.block_number, 120),
        "late follower did not replay through q-forming block {}",
        activation.block_number
    );
}

#[then("runtime traces prove proposal import and historical replay without on-chain calculation")]
fn runtime_traces_cover_ocomp_execution_paths(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized JobIntent for trace evidence");
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("q-forming activation for trace evidence");
    let validator_nodes = (0..4)
        .map(|index| format!("validator-{index}"))
        .collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(30);

    let (validator_markers, follower_markers) = loop {
        let validator_markers = validator_nodes
            .iter()
            .map(|node| {
                world
                    .localnet
                    .ocomp_runtime_trace_markers(node)
                    .unwrap_or_else(|error| panic!("parse {node} OCOMP trace: {error:#}"))
            })
            .collect::<Vec<_>>();
        let follower_markers = world
            .localnet
            .ocomp_runtime_trace_markers_at_validator_slot("follower", OCOMP_TRACE_FOLLOWER_SLOT)
            .expect("parse late follower OCOMP trace");
        let historical_request_observed = follower_markers.iter().any(|marker| {
            marker.kind == "terminal_request_committed"
                && marker.block_number == request.request_height
                && marker.origin.as_deref() == Some("canonical")
        });
        let historical_q_vote_observed = follower_markers.iter().any(|marker| {
            marker.kind == "result_vote_committed" && marker.block_number == activation.block_number
        });
        if historical_request_observed && historical_q_vote_observed {
            break (validator_markers, follower_markers);
        }
        assert!(
            Instant::now() < deadline,
            "late follower reached finality but did not expose both historical OCOMP boundaries"
        );
        sleep(Duration::from_millis(250));
    };

    let proposal_request_nodes = validator_nodes
        .iter()
        .zip(&validator_markers)
        .filter(|(_, markers)| {
            markers.iter().any(|marker| {
                marker.kind == "terminal_request_committed"
                    && marker.block_number == request.request_height
                    && marker.origin.as_deref() == Some("proposal")
            })
        })
        .map(|(node, _)| node.clone())
        .collect::<Vec<_>>();
    let canonical_request_nodes = validator_nodes
        .iter()
        .zip(&validator_markers)
        .filter(|(_, markers)| {
            markers.iter().any(|marker| {
                marker.kind == "terminal_request_committed"
                    && marker.block_number == request.request_height
                    && marker.origin.as_deref() == Some("canonical")
            })
        })
        .map(|(node, _)| node.clone())
        .collect::<Vec<_>>();
    let canonical_q_vote_nodes = validator_nodes
        .iter()
        .zip(&validator_markers)
        .filter(|(_, markers)| {
            markers.iter().any(|marker| {
                marker.kind == "result_vote_committed"
                    && marker.block_number == activation.block_number
            })
        })
        .map(|(node, _)| node.clone())
        .collect::<Vec<_>>();
    let forbidden_calculation_entries = validator_markers
        .iter()
        .flatten()
        .chain(follower_markers.iter())
        .filter(|marker| marker.kind == "forbidden_calculation_entry")
        .count();

    assert!(
        !proposal_request_nodes.is_empty(),
        "no committee node recorded proposal execution of the exact JobIntent block"
    );
    assert!(
        canonical_request_nodes.len() >= 3,
        "fewer than three importer nodes executed the exact JobIntent block: \
         {canonical_request_nodes:?}"
    );
    assert_eq!(
        canonical_q_vote_nodes.len(),
        4,
        "not every validator executed the q-forming result-vote block"
    );
    assert_eq!(
        forbidden_calculation_entries, 0,
        "an execution path entered legacy on-chain Lysis/Fidelity/Oracle calculation"
    );

    world.state.ocomp_execution_trace_observation = Some(OcompExecutionTraceObservationV1 {
        request_height: request.request_height,
        q_forming_height: activation.block_number,
        proposal_request_nodes,
        canonical_request_nodes,
        canonical_q_vote_nodes,
        historical_replay_node: "follower".to_owned(),
        historical_request_observed: true,
        historical_q_vote_observed: true,
        forbidden_calculation_entries: 0,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_completion_decision, completed_accountability_is_preserved,
        dynamic_deadline_decode_events, dynamic_deadline_ports, dynamic_deadline_storage_u64,
        dynamic_deadline_validate_accountability, dynamic_deadline_validate_checkpoints,
        dynamic_deadline_validate_penalties, dynamic_deadline_validate_receipt,
        dynamic_oracle_refresh_timestamp, dynamic_pre_restart_vote_baseline_ready,
        first_protocol_cycle_at_or_after_interval, joiner_restart_is_in_safe_early_epoch_window,
        monotonic_progress_decision, post_restart_convergence_target, public_vote_set_matches,
        retention_journal_root, singleton_participant_bitmap, BoundedCompletionDecision,
        DynamicDeadlineAccount, DynamicDeadlineMiss, ProgressWaitDecision,
        PublicVoteSetExpectation, RestartBarrierDecision, RestartBarrierState,
        DYNAMIC_OCOMP_RECOVERY_BLOCKS, OCOMP_CAPACITY_SUBMISSION_CONCURRENCY,
    };
    use crate::internal::eth;
    use crate::world::rpc::OcompPublicVoteAccountabilityV1;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use alloy_sol_types::SolEvent;

    #[test]
    fn case_one_dispatch_marker_requires_exact_job_and_production_event() {
        let job = B256::repeat_byte(0x31);
        let valid = format!(
            "2026-09-05T19:46:15.099744Z  INFO exex{{id=\"outbe-finalized\"}}: outbe_chain::ocomp_exex: embedded OCOMP computation started job_id={job:#x}"
        );
        assert_eq!(
            super::case_one_compute_started_line(&valid, job),
            Some(valid.as_str())
        );
        assert!(super::case_one_compute_started_line(&valid, B256::repeat_byte(0x32)).is_none());
        for invalid in [
            valid.replace(" INFO ", " WARN "),
            valid.replace("outbe_chain::ocomp_exex: ", "other_module: "),
            valid.replace("computation started", "local result arrived"),
            format!("{valid}0"),
            format!("{valid} reason=\"checkpoint_pruned\""),
        ] {
            assert!(
                super::case_one_compute_started_line(&invalid, job).is_none(),
                "{invalid}"
            );
        }
    }

    fn completed_accountability() -> OcompPublicVoteAccountabilityV1 {
        OcompPublicVoteAccountabilityV1 {
            job_id: alloy_primitives::B256::repeat_byte(0x11),
            result_validator_set_epoch: 7,
            result_committee_set_hash: alloy_primitives::B256::repeat_byte(0x22),
            result_ocomp_binding_hash: alloy_primitives::B256::repeat_byte(0x33),
            member_count: 4,
            quorum_threshold: 3,
            slot_validator_indexes: vec![0, 1, 2],
            slot_first_signatures: vec![(0, vec![0xa0]), (1, vec![0xa1]), (2, vec![0xa2])],
            quorum_result_digest: Some(alloy_primitives::B256::repeat_byte(0x44)),
            quorum_height: Some(92),
            quorum_signer_bitmap: Some(vec![0b0000_0111]),
            closed_height: None,
            timely_bitmap: None,
            matching_bitmap: None,
            divergent_bitmap: None,
            missing_bitmap: None,
            equivocation_bitmap: None,
        }
    }

    #[test]
    fn retention_evidence_reads_the_node_consensus_storage_root() {
        assert_eq!(
            retention_journal_root(std::path::Path::new("/scenario/validator-0/data")),
            std::path::PathBuf::from("/scenario/validator-0/data/consensus/ocomp_retention")
        );
    }

    #[test]
    fn completed_accountability_allows_only_monotonic_late_vote_extension() {
        let expected = completed_accountability();
        let mut extended = expected.clone();
        extended.slot_validator_indexes.push(3);
        extended.slot_first_signatures.push((3, vec![0xa3]));

        assert!(completed_accountability_is_preserved(&expected, &extended));

        let mut changed_quorum = extended.clone();
        changed_quorum.quorum_height = Some(93);
        assert!(!completed_accountability_is_preserved(
            &expected,
            &changed_quorum
        ));

        let mut replaced_signature = extended;
        replaced_signature.slot_first_signatures[0].1 = vec![0xff];
        assert!(!completed_accountability_is_preserved(
            &expected,
            &replaced_signature
        ));
    }

    #[test]
    fn ordinary_ocomp_completion_accepts_the_first_canonical_quorum() {
        assert!(public_vote_set_matches(
            PublicVoteSetExpectation::AnyQuorum,
            &[0, 1, 2],
            3,
        ));
        assert!(!public_vote_set_matches(
            PublicVoteSetExpectation::AnyQuorum,
            &[0, 1],
            3,
        ));
        assert!(!public_vote_set_matches(
            PublicVoteSetExpectation::Exact(&[1, 2, 3]),
            &[0, 1, 2],
            3,
        ));
    }

    #[test]
    fn dynamic_pre_restart_baseline_waits_for_all_three_job_b_votes() {
        assert!(!dynamic_pre_restart_vote_baseline_ready(2, 2, true));
        assert!(dynamic_pre_restart_vote_baseline_ready(2, 3, true));
        assert!(!dynamic_pre_restart_vote_baseline_ready(2, 3, false));
    }

    #[test]
    fn dynamic_oracle_refresh_is_staged_three_hours_before_daily_cycle() {
        const SECONDS_PER_DAY: u64 = 86_400;
        const REFRESH_HEADROOM: u64 = 3 * 60 * 60;
        let next_daily_cycle = 2 * SECONDS_PER_DAY + 1;

        let refresh_timestamp = dynamic_oracle_refresh_timestamp(next_daily_cycle);

        assert_eq!(refresh_timestamp, next_daily_cycle - REFRESH_HEADROOM);
        assert_eq!(refresh_timestamp / SECONDS_PER_DAY, 1);
        assert!(next_daily_cycle - refresh_timestamp < 6 * 60 * 60);
    }

    #[test]
    fn protocol_cycle_keeps_an_exact_hourly_processing_boundary() {
        assert_eq!(
            first_protocol_cycle_at_or_after_interval(172_800, 3_600),
            172_801
        );
    }

    #[test]
    fn protocol_cycle_rounds_a_non_boundary_processing_time_up() {
        assert_eq!(
            first_protocol_cycle_at_or_after_interval(172_801, 3_600),
            176_401
        );
    }

    #[test]
    fn capacity_completion_window_returns_as_soon_as_every_validator_is_done() {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(300);

        assert_eq!(
            bounded_completion_decision(true, started, deadline),
            BoundedCompletionDecision::Complete
        );
        assert_eq!(
            bounded_completion_decision(false, started, deadline),
            BoundedCompletionDecision::Continue
        );
    }

    #[test]
    fn capacity_completion_window_fails_only_when_the_shared_budget_expires() {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(300);

        assert_eq!(
            bounded_completion_decision(false, deadline, deadline),
            BoundedCompletionDecision::TimedOut
        );
        assert_eq!(
            bounded_completion_decision(true, deadline, deadline),
            BoundedCompletionDecision::Complete,
            "an observed completed result wins at the deadline boundary"
        );
    }

    #[test]
    fn capacity_population_submits_two_tributes_per_round() {
        assert_eq!(OCOMP_CAPACITY_SUBMISSION_CONCURRENCY, 2);
    }

    #[test]
    fn job_intent_schedule_waits_while_canonical_time_keeps_advancing() {
        let now = std::time::Instant::now();
        let progress_deadline = now + std::time::Duration::from_secs(120);

        assert_eq!(
            monotonic_progress_decision(200, 600, 199, now, progress_deadline),
            ProgressWaitDecision::Progressed
        );
        assert_eq!(
            monotonic_progress_decision(200, 600, 200, now, progress_deadline),
            ProgressWaitDecision::Waiting
        );
    }

    #[test]
    fn job_intent_schedule_reaches_boundary_and_rejects_a_real_stall() {
        let now = std::time::Instant::now();

        assert_eq!(
            monotonic_progress_decision(600, 600, 599, now, now),
            ProgressWaitDecision::Reached,
            "the exact schedule boundary wins over the stall deadline"
        );
        assert_eq!(
            monotonic_progress_decision(599, 600, 599, now, now),
            ProgressWaitDecision::Stalled
        );
    }

    #[test]
    fn restart_barrier_latches_the_transient_lifecycle_before_price_publication() {
        let decision =
            super::restart_barrier_decision(RestartBarrierState::default(), true, false, false);

        assert_eq!(
            decision,
            RestartBarrierDecision::Continue(RestartBarrierState {
                lifecycle_observed: true,
                publication_observed: false,
            })
        );
    }

    #[test]
    fn restart_barrier_completes_when_price_arrives_after_the_lifecycle_was_latched() {
        let decision = super::restart_barrier_decision(
            RestartBarrierState {
                lifecycle_observed: true,
                publication_observed: false,
            },
            false,
            true,
            true,
        );

        assert_eq!(decision, RestartBarrierDecision::Complete);
    }

    #[test]
    fn restart_barrier_requires_bounded_historical_proof_when_current_state_overshoots() {
        let decision =
            super::restart_barrier_decision(RestartBarrierState::default(), false, true, true);

        assert_eq!(
            decision,
            RestartBarrierDecision::HistoricalLifecycleRequired(RestartBarrierState {
                lifecycle_observed: false,
                publication_observed: true,
            })
        );
        assert!(super::fresh_wwd_lifecycle_overshot(
            8,
            &[Some(6), Some(6), Some(6), Some(6)]
        ));
        assert!(super::fresh_wwd_lifecycle_overshot(
            2,
            &[Some(3), Some(3), Some(3), Some(3)]
        ));
    }

    #[test]
    fn historical_lifecycle_scan_is_descending_inclusive_and_bounded() {
        assert_eq!(
            super::historical_lifecycle_scan_heights(80, 84),
            Ok(vec![84, 83, 82, 81, 80])
        );
        assert_eq!(
            super::historical_lifecycle_scan_heights(84, 80),
            Err("current finalized height 80 precedes restart minimum 84".to_string())
        );
        assert_eq!(
            super::historical_lifecycle_scan_heights(1, 258),
            Err("historical lifecycle scan spans 258 blocks; maximum is 256".to_string())
        );
    }

    #[test]
    fn restart_barrier_cannot_complete_from_price_publication_alone() {
        let decision =
            super::restart_barrier_decision(RestartBarrierState::default(), false, false, true);

        assert_eq!(
            decision,
            RestartBarrierDecision::Continue(RestartBarrierState {
                lifecycle_observed: false,
                publication_observed: true,
            })
        );
    }

    #[test]
    fn dynamic_deadline_requires_all_five_distinct_observers() {
        assert_eq!(
            dynamic_deadline_ports(vec![10, 11, 12, 13], 14).unwrap(),
            vec![10, 11, 12, 13, 14]
        );
        assert!(dynamic_deadline_ports(vec![10, 11, 12], 14).is_err());
        assert!(dynamic_deadline_ports(vec![10, 11, 12, 13], 13).is_err());
        assert!(dynamic_deadline_ports(vec![10, 10, 12, 13], 14).is_err());
    }

    fn dynamic_deadline_fixture() -> (
        Vec<Address>,
        [DynamicDeadlineAccount; 5],
        [DynamicDeadlineMiss; 2],
        [crate::world::rpc::FinalizedCheckpoint; 2],
        serde_json::Value,
    ) {
        let members: Vec<_> = (1..=5).map(Address::repeat_byte).collect();
        // Canonical native units, with a remainder to exercise floor(bonded/10).
        let bonded =
            U256::from(100_000_u64) * U256::from(1_000_000_000_000_000_000_u64) + U256::from(9);
        let before = DynamicDeadlineAccount {
            bonded,
            mirrored: bonded,
            total_staked: bonded * U256::from(5),
            staking_balance: bonded * U256::from(5),
            status: 2,
            ordinary_slash_count: 7,
            ocomp_miss_count: 0,
            ocomp_recovery_deadline: 0,
            active: members.clone(),
            participants: members.clone(),
        };
        let slash = bonded / U256::from(10);
        let mut after = before.clone();
        after.bonded -= slash;
        after.mirrored -= slash;
        after.total_staked -= slash;
        after.staking_balance -= slash;
        let mut states = [before, after.clone(), after.clone(), after.clone(), after];
        for (ordinal, state) in states.iter_mut().enumerate().skip(1) {
            state.ocomp_miss_count = [0, 1, 1, 2, 2][ordinal];
            state.ocomp_recovery_deadline = 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS;
        }
        let checkpoints = [1000, 1200].map(|height| crate::world::rpc::FinalizedCheckpoint {
            height,
            block_hash: B256::repeat_byte(if height == 1000 { 10 } else { 12 }),
            state_root: B256::repeat_byte(if height == 1000 { 20 } else { 22 }),
        });
        let jobs = [B256::repeat_byte(1), B256::repeat_byte(2)];
        let logs = serde_json::Value::Array(
            (0..2)
                .map(|ordinal| {
                    let event = eth::IMetadosis::OcompVoteMissed {
                        validator: members[3],
                        jobId: jobs[ordinal],
                        missCount: (ordinal + 1) as u64,
                        slashedBonded: if ordinal == 0 { slash } else { U256::ZERO },
                        recoveryDeadline: 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS,
                        firstInWindow: ordinal == 0,
                    };
                    let data = event.encode_log_data();
                    serde_json::json!({
                        "address": crate::internal::addresses::WWD_ADDR,
                        "topics": data.topics(), "data": data.data, "removed": false,
                        "blockNumber": format!("0x{:x}", checkpoints[ordinal].height),
                        "blockHash": checkpoints[ordinal].block_hash,
                        "transactionHash": B256::repeat_byte(ordinal as u8 + 30), "logIndex": "0x0",
                    })
                })
                .collect(),
        );
        let events = dynamic_deadline_decode_events(&logs, members[3], jobs, checkpoints).unwrap();
        (members, states, events, checkpoints, logs)
    }

    #[test]
    fn dynamic_deadlines_prove_one_real_unit_slash_and_an_unchanged_repeat_window() {
        let (members, states, events, _, _) = dynamic_deadline_fixture();
        dynamic_deadline_validate_penalties(&states, &events, &members, [1000, 1200], 1201)
            .unwrap();
        assert_eq!(
            events[0].slashed_bonded,
            U256::from(10_000_u64) * U256::from(1_000_000_000_000_000_000_u64)
        );
        assert_eq!(
            states[1].ordinary_slash_count, 7,
            "OCOMP miss count is not ordinary slashCount"
        );
        assert_eq!(events[1].miss_count, 2);
        assert_eq!(events[1].slashed_bonded, U256::ZERO);
        assert_eq!(events[0].recovery_deadline, 44_200);
        assert_eq!(events[1].recovery_deadline, 44_200);
    }

    #[test]
    fn dynamic_deadline_storage_requires_a_complete_u64_word() {
        for expected in [0, 1, 2, 44_200, u64::MAX] {
            let word = B256::from(U256::from(expected).to_be_bytes::<32>());
            assert_eq!(
                dynamic_deadline_storage_u64(&serde_json::json!(word)).unwrap(),
                expected
            );
        }
        for wrong in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!("0x"),
            serde_json::json!("0x01"),
            serde_json::json!("not-hex"),
            serde_json::json!(1),
            serde_json::json!(B256::from(
                (U256::from(u64::MAX) + U256::from(1)).to_be_bytes::<32>()
            )),
        ] {
            assert!(
                dynamic_deadline_storage_u64(&wrong).is_err(),
                "accepted {wrong}"
            );
        }
    }

    #[test]
    fn dynamic_deadlines_reject_incomplete_durable_recovery_transitions() {
        let (members, states, events, _, _) = dynamic_deadline_fixture();
        for ordinal in 0..5 {
            for deadline_defect in [false, true] {
                let mut wrong = states.clone();
                if deadline_defect {
                    wrong[ordinal].ocomp_recovery_deadline ^= 1;
                } else {
                    wrong[ordinal].ocomp_miss_count ^= 1;
                }
                assert!(
                    dynamic_deadline_validate_penalties(
                        &wrong,
                        &events,
                        &members,
                        [1000, 1200],
                        1201,
                    )
                    .is_err(),
                    "accepted durable point {ordinal}, deadline={deadline_defect}"
                );
            }
        }
        let mut lost_window = states.clone();
        lost_window[4].ocomp_recovery_deadline = 0;
        assert!(dynamic_deadline_validate_penalties(
            &lost_window,
            &events,
            &members,
            [1000, 1200],
            1201,
        )
        .is_err());
    }

    #[test]
    fn dynamic_deadlines_require_successful_exact_system_receipts() {
        let (_, _, events, _, logs) = dynamic_deadline_fixture();
        for (ordinal, event) in events.iter().enumerate() {
            let receipt = serde_json::json!({
                "status": "0x1", "blockNumber": format!("0x{:x}", event.height),
                "blockHash": event.block_hash, "transactionHash": event.transaction_hash,
                "logs": [logs[ordinal]],
            });
            dynamic_deadline_validate_receipt(&receipt, event).unwrap();
            for (field, value) in [
                ("status", serde_json::json!("0x0")),
                ("status", serde_json::Value::Null),
                ("status", serde_json::json!("0x10000000000000000")),
                ("blockNumber", serde_json::json!("0x0")),
                ("blockHash", serde_json::json!(B256::repeat_byte(99))),
                ("transactionHash", serde_json::json!(B256::repeat_byte(99))),
                ("transactionHash", serde_json::Value::Null),
                ("logs", serde_json::json!([])),
                ("logs", serde_json::Value::Null),
                ("logs", serde_json::json!([logs[ordinal], logs[ordinal]])),
            ] {
                let mut wrong = receipt.clone();
                wrong[field] = value;
                assert!(
                    dynamic_deadline_validate_receipt(&wrong, event).is_err(),
                    "accepted receipt defect {field}"
                );
            }
            for (field, value) in [
                (
                    "address",
                    serde_json::json!(crate::internal::addresses::VS_ADDR),
                ),
                ("topics", serde_json::json!([])),
                ("data", serde_json::json!("0x00")),
                ("removed", serde_json::json!(true)),
                ("removed", serde_json::Value::Null),
                ("logIndex", serde_json::json!("0x1")),
                ("logIndex", serde_json::Value::Null),
                ("blockNumber", serde_json::json!("0x0")),
                ("blockHash", serde_json::json!(B256::repeat_byte(99))),
                ("transactionHash", serde_json::json!(B256::repeat_byte(99))),
            ] {
                let mut wrong = receipt.clone();
                wrong["logs"][0][field] = value;
                assert!(
                    dynamic_deadline_validate_receipt(&wrong, event).is_err(),
                    "accepted receipt log defect {field}"
                );
            }
            let mut noncanonical = receipt.clone();
            let mut data = serde_json::from_value::<Bytes>(noncanonical["logs"][0]["data"].clone())
                .unwrap()
                .to_vec();
            data[127] = 2;
            noncanonical["logs"][0]["data"] = serde_json::json!(Bytes::from(data));
            assert!(dynamic_deadline_validate_receipt(&noncanonical, event).is_err());
            assert!(dynamic_deadline_validate_receipt(&serde_json::Value::Null, event).is_err());
        }
    }

    #[test]
    fn dynamic_deadlines_reject_wrong_event_source_identity_or_canonical_point() {
        let (members, _, events, checkpoints, logs) = dynamic_deadline_fixture();
        let jobs = events.clone().map(|event| event.job_id);
        let replacements = [
            (
                "address",
                serde_json::json!(crate::internal::addresses::VS_ADDR),
            ),
            ("removed", serde_json::json!(true)),
            ("removed", serde_json::Value::Null),
            ("blockNumber", serde_json::json!("0x3e9")),
            ("blockHash", serde_json::json!(B256::repeat_byte(99))),
            ("transactionHash", serde_json::Value::Null),
            ("logIndex", serde_json::Value::Null),
            ("data", serde_json::json!("0x00")),
            ("topics", serde_json::json!([])),
        ];
        for (field, value) in replacements {
            let mut wrong = logs.clone();
            wrong[0][field] = value;
            assert!(
                dynamic_deadline_decode_events(&wrong, members[3], jobs, checkpoints).is_err(),
                "accepted wrong {field}"
            );
        }
        for topic in 0..3 {
            let mut wrong = logs.clone();
            wrong[0]["topics"][topic] = serde_json::json!(B256::repeat_byte(90));
            assert!(
                dynamic_deadline_decode_events(&wrong, members[3], jobs, checkpoints).is_err(),
                "accepted wrong topic {topic}"
            );
        }
        let mut invalid_bool = logs.clone();
        let mut data: Vec<u8> = serde_json::from_value::<Bytes>(invalid_bool[0]["data"].clone())
            .unwrap()
            .to_vec();
        data[127] = 2;
        invalid_bool[0]["data"] = serde_json::json!(Bytes::from(data));
        assert!(
            dynamic_deadline_decode_events(&invalid_bool, members[3], jobs, checkpoints).is_err()
        );
        for rows in [
            vec![],
            vec![logs[0].clone()],
            vec![logs[0].clone(), logs[0].clone()],
            vec![logs[0].clone(), logs[1].clone(), logs[1].clone()],
        ] {
            assert!(dynamic_deadline_decode_events(
                &serde_json::Value::Array(rows),
                members[3],
                jobs,
                checkpoints
            )
            .is_err());
        }
        assert!(dynamic_deadline_decode_events(
            &serde_json::json!({}),
            members[3],
            jobs,
            checkpoints
        )
        .is_err());
        // RPC ordering is not identity: both exact canonical events still agree.
        let reversed = serde_json::json!([logs[1], logs[0]]);
        assert_eq!(
            dynamic_deadline_decode_events(&reversed, members[3], jobs, checkpoints).unwrap(),
            events
        );
    }

    #[test]
    fn dynamic_deadlines_reject_repeat_slash_count_reset_and_deadline_extension() {
        let (members, states, events, _, _) = dynamic_deadline_fixture();
        for ordinal in 0..2 {
            for defect in 0..6 {
                let mut wrong = events.clone();
                match defect {
                    0 => wrong[ordinal].miss_count += 1,
                    1 => wrong[ordinal].first_in_window = !wrong[ordinal].first_in_window,
                    2 => wrong[ordinal].recovery_deadline += 1,
                    3 => wrong[ordinal].slashed_bonded += U256::from(1),
                    4 => wrong[ordinal].validator = members[2],
                    5 => wrong[ordinal].height += 1,
                    _ => unreachable!(),
                }
                assert!(
                    dynamic_deadline_validate_penalties(
                        &states,
                        &wrong,
                        &members,
                        [1000, 1200],
                        1201
                    )
                    .is_err(),
                    "accepted event {ordinal}, defect {defect}"
                );
            }
        }
    }

    #[test]
    fn dynamic_deadlines_reject_unrelated_jail_or_any_unaccounted_burn() {
        let (members, states, events, _, _) = dynamic_deadline_fixture();
        for point in 0..5 {
            for defect in 0..8 {
                let mut wrong = states.clone();
                match defect {
                    0 => wrong[point].bonded += U256::from(1),
                    1 => wrong[point].mirrored += U256::from(1),
                    2 => wrong[point].total_staked += U256::from(1),
                    3 => wrong[point].staking_balance += U256::from(1),
                    4 => wrong[point].ordinary_slash_count += 1,
                    5 => wrong[point].status = 6,
                    6 => {
                        wrong[point].active.remove(3);
                    }
                    7 => {
                        wrong[point].participants.remove(4);
                    }
                    _ => unreachable!(),
                }
                assert!(
                    dynamic_deadline_validate_penalties(
                        &wrong,
                        &events,
                        &members,
                        [1000, 1200],
                        1201
                    )
                    .is_err(),
                    "accepted accounting point {point}, defect {defect}"
                );
            }
        }
        let mut duplicate_members = members.clone();
        duplicate_members[4] = members[3];
        assert!(dynamic_deadline_validate_penalties(
            &states,
            &events,
            &duplicate_members,
            [1000, 1200],
            1201
        )
        .is_err());
    }

    #[test]
    fn dynamic_deadlines_do_not_claim_the_full_recovery_gate() {
        let (members, states, events, _, _) = dynamic_deadline_fixture();
        for (deadlines, height) in [
            ([1000, 1200], 1200),
            ([1000, 1200], 44_200),
            ([1000, 1200], 44_201),
            ([1000, 1000], 1201),
            ([0, 1200], 1201),
            ([u64::MAX - 1, u64::MAX], u64::MAX),
        ] {
            assert!(dynamic_deadline_validate_penalties(
                &states, &events, &members, deadlines, height
            )
            .is_err());
        }
    }

    #[test]
    fn dynamic_deadlines_use_overflow_safe_floor_slashing_and_reject_underfunding() {
        let (members, mut states, mut events, _, _) = dynamic_deadline_fixture();
        for field in 0..2 {
            let mut wrong = states.clone();
            if field == 0 {
                wrong[0].total_staked = U256::ZERO;
            } else {
                wrong[0].staking_balance = U256::ZERO;
            }
            assert!(dynamic_deadline_validate_penalties(
                &wrong,
                &events,
                &members,
                [1000, 1200],
                1201
            )
            .is_err());
        }
        // Exercise the evaluator's U256 arithmetic without multiplying MAX by 10.
        states[0].bonded = U256::MAX;
        states[0].mirrored = U256::MAX;
        states[0].total_staked = U256::MAX;
        states[0].staking_balance = U256::MAX;
        let slash = U256::MAX / U256::from(10);
        let mut after = states[0].clone();
        after.bonded -= slash;
        after.mirrored -= slash;
        after.total_staked -= slash;
        after.staking_balance -= slash;
        for (ordinal, state) in states.iter_mut().enumerate().skip(1) {
            *state = after.clone();
            state.ocomp_miss_count = [0, 1, 1, 2, 2][ordinal];
            state.ocomp_recovery_deadline = 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS;
        }
        events[0].slashed_bonded = slash;
        dynamic_deadline_validate_penalties(&states, &events, &members, [1000, 1200], 1201)
            .unwrap();
    }

    #[test]
    fn dynamic_deadlines_require_every_finalized_hash_root_not_a_filtered_subset() {
        let ports = [10, 11, 12, 13, 14];
        let checkpoint = crate::world::rpc::FinalizedCheckpoint {
            height: 1201,
            block_hash: B256::repeat_byte(7),
            state_root: B256::repeat_byte(8),
        };
        let observed: Vec<_> = ports.iter().map(|&port| (port, 1202, checkpoint)).collect();
        assert_eq!(
            dynamic_deadline_validate_checkpoints(&ports, 1201, &observed).unwrap(),
            checkpoint
        );
        for index in 0..5 {
            for defect in 0..5 {
                let mut wrong = observed.clone();
                match defect {
                    0 => {
                        wrong.remove(index);
                    }
                    1 => wrong[index].0 = 99,
                    2 => wrong[index].1 = 1200,
                    3 => wrong[index].2.block_hash = B256::repeat_byte(9),
                    4 => wrong[index].2.state_root = B256::repeat_byte(9),
                    _ => unreachable!(),
                }
                assert!(dynamic_deadline_validate_checkpoints(&ports, 1201, &wrong).is_err());
            }
        }
        assert!(dynamic_deadline_validate_checkpoints(&ports, 1200, &observed).is_err());
    }

    fn dynamic_accountability_fixture(
        members: u16,
        quorum: u16,
        missing: u16,
    ) -> (
        crate::world::rpc::OcompPublicVoteAccountabilityV1,
        crate::world::rpc::OcompPublicVoteAccountabilityV1,
    ) {
        let slots: Vec<_> = (0..members).filter(|index| *index != missing).collect();
        let mut timely = vec![0_u8; usize::from(members).div_ceil(8)];
        for index in &slots {
            timely[usize::from(index / 8)] |= 1 << (index % 8);
        }
        let baseline = crate::world::rpc::OcompPublicVoteAccountabilityV1 {
            job_id: B256::repeat_byte(1),
            result_validator_set_epoch: 2,
            result_committee_set_hash: B256::repeat_byte(2),
            result_ocomp_binding_hash: B256::repeat_byte(3),
            member_count: members,
            quorum_threshold: quorum,
            slot_first_signatures: slots
                .iter()
                .map(|&index| (index, vec![index as u8 + 1; 64]))
                .collect(),
            slot_validator_indexes: slots,
            quorum_result_digest: Some(B256::repeat_byte(4)),
            quorum_height: Some(900),
            quorum_signer_bitmap: Some(timely.clone()),
            closed_height: None,
            timely_bitmap: None,
            matching_bitmap: None,
            divergent_bitmap: None,
            missing_bitmap: None,
            equivocation_bitmap: None,
        };
        let mut closed = baseline.clone();
        closed.closed_height = Some(1000);
        closed.timely_bitmap = Some(timely.clone());
        closed.matching_bitmap = Some(timely);
        closed.divergent_bitmap = Some(vec![0]);
        closed.equivocation_bitmap = Some(vec![0]);
        closed.missing_bitmap = Some(singleton_participant_bitmap(members, missing));
        (baseline, closed)
    }

    #[test]
    fn dynamic_deadlines_preserve_both_historical_quorums_and_missing_snapshot_indexes() {
        // Missing snapshot index is not assumed to equal validator directory 3.
        for (members, quorum, missing) in [(4, 3, 1), (5, 4, 4)] {
            let (baseline, closed) = dynamic_accountability_fixture(members, quorum, missing);
            let slots = baseline.slot_validator_indexes.clone();
            dynamic_deadline_validate_accountability(
                &closed,
                &baseline,
                &slots,
                1000,
                (members, quorum),
            )
            .unwrap();
            for defect in 0..13 {
                let mut wrong = closed.clone();
                match defect {
                    0 => wrong.member_count += 1,
                    1 => wrong.quorum_threshold -= 1,
                    2 => wrong.job_id = B256::ZERO,
                    3 => wrong.result_validator_set_epoch += 1,
                    4 => wrong.result_committee_set_hash = B256::ZERO,
                    5 => wrong.result_ocomp_binding_hash = B256::ZERO,
                    6 => wrong.quorum_result_digest = Some(B256::ZERO),
                    7 => wrong.closed_height = Some(1001),
                    8 => wrong.missing_bitmap = Some(vec![0]),
                    9 => wrong.slot_first_signatures[0].1[0] ^= 1,
                    10 => {
                        wrong.slot_validator_indexes.pop();
                    }
                    11 => wrong.quorum_height = None,
                    12 => wrong.quorum_signer_bitmap = None,
                    _ => unreachable!(),
                }
                assert!(
                    dynamic_deadline_validate_accountability(
                        &wrong,
                        &baseline,
                        &slots,
                        1000,
                        (members, quorum)
                    )
                    .is_err(),
                    "accepted changed historical accountability {defect}"
                );
            }
        }
    }

    #[test]
    fn restart_convergence_advances_beyond_the_most_advanced_peer() {
        assert_eq!(post_restart_convergence_target([99, 104, 107, 107]), 108);
    }

    #[test]
    fn joiner_restart_waits_out_an_imminent_dkg_activation() {
        assert!(!joiner_restart_is_in_safe_early_epoch_window(78, 78, 20));
        assert!(!joiner_restart_is_in_safe_early_epoch_window(80, 80, 20));
        assert!(joiner_restart_is_in_safe_early_epoch_window(81, 81, 20));
        assert!(joiner_restart_is_in_safe_early_epoch_window(83, 83, 20));
        assert!(joiner_restart_is_in_safe_early_epoch_window(90, 90, 20));
        assert!(!joiner_restart_is_in_safe_early_epoch_window(91, 91, 20));
    }

    #[test]
    fn joiner_restart_window_derives_from_the_chain_epoch() {
        assert!(!joiner_restart_is_in_safe_early_epoch_window(118, 118, 120));
        assert!(joiner_restart_is_in_safe_early_epoch_window(121, 121, 120));
        assert!(joiner_restart_is_in_safe_early_epoch_window(122, 122, 120));
    }

    #[test]
    fn joiner_restart_accepts_an_early_pre_freeze_handover_window() {
        assert!(joiner_restart_is_in_safe_early_epoch_window(83, 83, 300));
        assert!(!joiner_restart_is_in_safe_early_epoch_window(151, 151, 300));
    }

    #[test]
    fn joiner_restart_requires_the_full_node_to_finalize_the_boundary_block() {
        assert!(!joiner_restart_is_in_safe_early_epoch_window(101, 100, 20));
        assert!(joiner_restart_is_in_safe_early_epoch_window(101, 101, 20));
    }
}
