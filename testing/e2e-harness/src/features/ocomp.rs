//! OCM-24 process-topology acceptance steps.
//!
//! The scenario extends the normal localnet lifecycle and launches only the
//! production `outbe-ocomp` executable. It cannot construct jobs, results,
//! roots or chain state.

use std::{
    str::FromStr as _,
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use cucumber::{given, then, when};
use outbe_common::WorldwideDay;
use outbe_ocomp_protocol::{
    profile::poc_schema_limits,
    result::LysisResultV1,
    state::{OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    system_carrier::{MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS, OCOMP_SYSTEM_CARRIER_GAS_LIMIT},
    vote::ResultVoteV1,
};

use crate::features::common::{
    bootstrap_final_ocomp_localnet, bootstrap_localnet, start_bootstrapped_localnet,
};
use crate::internal::eth;
use crate::world::localnet::StartOpts;
use crate::world::ocomp::{
    OcompForkMismatchEvidenceV1, OcompForkRestartEvidenceV1, OcompMeasurementForkV1,
    OcompProcessFault, OcompProcessRole,
};
use crate::world::ocomp::{
    OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS, OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS,
    OCOMP_TEST_EPOCH_LENGTH_BLOCKS,
};
use crate::world::state::{
    MetadosisFinalizedPointV1, MetadosisFreshLifecycleObservationV1, MetadosisTimeControlEpochV1,
    OcompExecutionTraceObservationV1,
};
use crate::world::World;

const OCOMP_CAPACITY_TRIBUTE_COUNT: usize = 257;
const OCOMP_CAPACITY_SUBMISSION_CONCURRENCY: usize = 8;
const OCOMP_TRACE_FOLLOWER_SLOT: usize = 14;
// The immutable Final fixture closes its public offering 360 logical seconds
// after genesis. A one-Tribute scenario reaches this step much sooner than the
// 257-Tribute capacity scenario, so its bounded wait must include the remaining
// offering interval plus finalization/request publication slack.
const OCOMP_FINAL_JOB_REQUEST_TIMEOUT_SECS: u64 = 300;
// A WWD begins at 10:00 UTC on the previous civil date (UTC+14 midnight), while
// the block-1 bootstrap derives its first key from the raw UTC civil date.
// Starting 15 hours into the WWD places block 1 at 01:00 UTC on that same key:
// both date conventions select the fixture WWD and it remains inside FORMING.
const METADOSIS_INITIAL_WWD_ELAPSED_SECS: u64 = 15 * 3_600;

#[given("a fresh four-validator OCOMP measurement localnet")]
fn fresh_ocomp_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, None);
}

#[given("a fresh four-validator OCOMP public measurement localnet")]
fn fresh_ocomp_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0));
}

#[given("a fresh four-validator OCOMP public capacity localnet")]
fn fresh_ocomp_public_capacity_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(OCOMP_CAPACITY_TRIBUTE_COUNT));
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
        &[(
            "TESTNET_EPOCH_LENGTH_BLOCKS",
            OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
        )],
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
    };
    launch_prepared_ocomp(world, &mut start_opts, &prepared, true);
    wait_for_finalized_ocomp_activation(world);
}

#[given("the canonical four-validator OCOMP Final devnet")]
fn canonical_ocomp_final_devnet(world: &mut World) {
    start_canonical_ocomp_final_devnet(world, true, false);
}

#[given("the canonical four-validator OCOMP Final devnet before H")]
fn canonical_ocomp_final_devnet_before_h(world: &mut World) {
    start_canonical_ocomp_final_devnet(world, false, true);
}

fn start_canonical_ocomp_final_devnet(
    world: &mut World,
    wait_for_activation: bool,
    activate_workers: bool,
) {
    bootstrap_final_ocomp_localnet(world, 6);
    world.state.ocomp_capacity_tribute_private_keys = world
        .ocomp
        .final_capacity_tribute_private_keys(OCOMP_CAPACITY_TRIBUTE_COUNT)
        .expect("derive funded canonical capacity owners");
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs();
    let mut start_opts = StartOpts {
        voting_window: Some(6),
        unix_time_offset_secs: Some(
            world
                .localnet
                .ocomp_final_clock_offset(now_secs)
                .expect("derive canonical OCOMP logical clock"),
        ),
        genesis_timestamp_pre_shifted: true,
    };
    let prepared = world
        .ocomp
        .prepare_final_fork_install()
        .expect("load canonical OCOMP Final install");
    launch_prepared_ocomp(world, &mut start_opts, &prepared, activate_workers);
    if wait_for_activation {
        wait_for_finalized_ocomp_activation(world);
    }
}

fn start_ocomp_measurement_localnet(
    world: &mut World,
    public_capacity_tribute_count: Option<usize>,
) {
    let shorten_public_day = public_capacity_tribute_count.is_some();
    bootstrap_localnet(
        world,
        6,
        &[(
            "TESTNET_EPOCH_LENGTH_BLOCKS",
            OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
        )],
    );
    let mut start_opts = if shorten_public_day {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_secs();
        let boundary_lead_secs = if public_capacity_tribute_count == Some(0) {
            120
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
    world
        .localnet
        .bind_tee_genesis()
        .expect("bind canonical TEE genesis after installing the mandatory OCOMP manifest");
    launch_prepared_ocomp(
        world,
        &mut start_opts,
        &measurement_fork,
        !shorten_public_day,
    );
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
        .launch_joiner_full_node(index, 0, 0, &ocomp_args)
        .expect("launch the fifth slot without validator credentials");
    world
        .ocomp
        .start_keyless_full_node_roles(validator_index)
        .expect("start non-voting FullNode Lysis follower and snapshot exporter");
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
    assert!(!validator_dir.join("signing-key.hex").exists());
    assert!(world.ocomp.process_records().iter().any(|record| {
        record.validator_index == Some(u8::try_from(index).expect("joiner index fits u8"))
            && record.role == OcompProcessRole::Follower
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
        .provision_joiner(index)
        .expect("provision BLS and OCOMP registration while FullNode keeps syncing");
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
        if joiner_restart_is_safely_after_boundary(
            primary_finalized,
            follower_finalized,
            epoch_length,
        ) && follower_state_matches
        {
            break;
        }
        assert!(
            Instant::now() < safe_window_deadline,
            "FullNode did not reach a safe post-boundary validator admission window"
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
    assert!(!world.rpc.is_participant(primary, &address));
    world
        .rpc
        .confirm_ready(&key)
        .expect("confirm OCOMP registration and readiness");
    assert!(
        world.rpc.wait_participant(primary, &address, 70),
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

fn joiner_restart_is_safely_after_boundary(
    primary_finalized_height: u64,
    follower_finalized_height: u64,
    epoch_length: u64,
) -> bool {
    assert!(epoch_length > 0, "epoch length is a consensus precondition");
    primary_finalized_height / epoch_length == follower_finalized_height / epoch_length
        && matches!(primary_finalized_height % epoch_length, 1 | 2)
        && matches!(follower_finalized_height % epoch_length, 1 | 2)
}

#[then("the certified boundary adds exactly one fifth OCOMP validator domain")]
fn certified_boundary_adds_fifth_ocomp_domain(world: &mut World) {
    let primary = world.validators.primary_port();
    assert_eq!(world.rpc.active_count(primary), Some(5));
    let evidence = world.ocomp.evidence_snapshot().expect("dynamic topology");
    assert_eq!(evidence.domain_roots.len(), 5);
    for role in [
        OcompProcessRole::Supervisor,
        OcompProcessRole::SnapshotExporter,
        OcompProcessRole::Worker,
    ] {
        assert!(world.ocomp.process_records().iter().any(|record| {
            record.validator_index == Some(4)
                && record.role == role
                && record.stopped_at_millis.is_none()
        }));
    }
}

#[then("job B opens with five members and quorum four while job A remains four of three")]
fn job_b_uses_the_new_snapshot_while_job_a_keeps_the_old_one(world: &mut World) {
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
    let deadline = Instant::now() + Duration::from_secs(OCOMP_FINAL_JOB_REQUEST_TIMEOUT_SECS);

    let (job_b_request, job_a_record, job_b_record, job_a_votes, job_b_votes) = loop {
        let requests = ports
            .iter()
            .copied()
            .map(|port| {
                world.rpc.finalized_ocomp_job_request_for_worldwide_day_on(
                    port,
                    job_a_request.request_height + 1,
                    job_b_wwd,
                )
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
                    if job_a_accountability.iter().all(Option::is_some)
                        && job_b_accountability.iter().all(|accountability| {
                            accountability.as_ref().is_some_and(|accountability| {
                                accountability.slot_validator_indexes.contains(&4)
                            })
                        })
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
                        break (
                            request,
                            job_a_record,
                            job_b_record,
                            job_a_votes,
                            job_b_votes,
                        );
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "job B did not open and accept the fifth validator's vote before timeout"
        );
        sleep(Duration::from_millis(500));
    };

    assert_eq!(job_a_record.intent.result_member_count, 4);
    assert_eq!(job_a_record.intent.result_quorum_threshold, 3);
    assert_eq!(job_a_votes.member_count, 4);
    assert_eq!(job_a_votes.quorum_threshold, 3);
    assert!(
        !job_a_votes.slot_validator_indexes.contains(&4),
        "the fifth validator must not vote on job A's historical snapshot"
    );

    assert_eq!(job_b_record.intent.wwd, job_b_wwd);
    assert_eq!(job_b_record.intent.result_member_count, 5);
    assert_eq!(job_b_record.intent.result_quorum_threshold, 4);
    assert_eq!(job_b_votes.member_count, 5);
    assert_eq!(job_b_votes.quorum_threshold, 4);
    assert!(
        job_b_votes.slot_validator_indexes.contains(&4),
        "the fifth validator's public vote must be accepted for job B"
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

fn dynamic_job_record(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
) -> OcompJobRecordV1 {
    world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .expect("dynamic OCOMP job record")
}

fn full_node_local_result_path(world: &World, job_id: B256) -> std::path::PathBuf {
    world
        .validators
        .data_dir(world.validators.joiner_index())
        .join("ocomp-local-results-v1")
        .join(format!(
            "{}.lysis-result-v1.ocb1",
            hex::encode(job_id.as_slice())
        ))
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

    let accountability = world
        .rpc
        .finalized_ocomp_vote_accountability_on(world.validators.primary_port(), job_id)
        .expect("job A accountability while FullNode is non-voting");
    assert!(
        !accountability.slot_validator_indexes.contains(&4),
        "keyless FullNode must compute job A without submitting a validator vote"
    );
}

#[when("validator 2 OCOMP supervisor restarts and completes both pinned quorums")]
fn validator_two_restarts_and_completes_dynamic_quorums(world: &mut World) {
    world
        .ocomp
        .restart_supervisor(2)
        .expect("restart validator-2 Supervisor for both pinned jobs");
    let requests = world.state.ocomp_dynamic_job_requests.clone();
    assert_eq!(requests.len(), 2, "job A and job B requests");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let ready = requests.iter().enumerate().all(|(ordinal, request)| {
            let record = dynamic_job_record(world, request);
            let Some(finalized) = record.finalized.as_ref() else {
                return false;
            };
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(
                    world.validators.primary_port(),
                    finalized.job_id,
                )
                .is_some_and(|accountability| {
                    accountability.quorum_result_digest.is_some()
                        && accountability.slot_validator_indexes.contains(&2)
                        && if ordinal == 0 {
                            accountability.slot_validator_indexes == [0, 1, 2]
                        } else {
                            accountability.slot_validator_indexes == [0, 1, 2, 4]
                        }
                })
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "validator-2 did not complete both historical/current snapshot quorums"
        );
        sleep(Duration::from_millis(250));
    }
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

#[then("both deadlines record validator 3 missing and keep the chain live after jailing it")]
fn dynamic_deadlines_jail_only_the_active_missing_validator(world: &mut World) {
    const JAILED: u64 = 6;

    let requests = world.state.ocomp_dynamic_job_requests.clone();
    assert_eq!(requests.len(), 2, "job A and job B requests");
    let last_deadline = requests
        .iter()
        .map(|request| request.deadline_height)
        .max()
        .expect("dynamic job deadlines");
    let primary = world.validators.primary_port();
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, last_deadline.saturating_add(1), 900),
        "chain did not remain live past both dynamic OCOMP deadlines"
    );

    for (ordinal, request) in requests.iter().enumerate() {
        let record = dynamic_job_record(world, request);
        let job_id = record
            .finalized
            .as_ref()
            .expect("dynamic finalized intent")
            .job_id;
        let observed = world
            .validators
            .committee_ports()
            .into_iter()
            .chain(std::iter::once(
                world.validators.http_port(world.validators.joiner_index()),
            ))
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_vote_accountability_on(port, job_id)
            })
            .collect::<Vec<_>>();
        let first = observed[0]
            .clone()
            .expect("closed dynamic accountability on primary");
        assert!(
            observed
                .iter()
                .all(|candidate| candidate.as_ref() == Some(&first)),
            "nodes disagree on closed job {} accountability",
            ordinal + 1
        );
        assert_eq!(first.closed_height, Some(request.deadline_height));
        assert_eq!(first.missing_bitmap, Some(vec![0b0000_1000]));
        assert_eq!(
            first.slot_validator_indexes,
            if ordinal == 0 {
                vec![0, 1, 2]
            } else {
                vec![0, 1, 2, 4]
            }
        );
    }

    let validator_three = world.validators.get(3);
    let validator_three_key = validator_three
        .evm_key()
        .expect("validator-3 EVM key for status lookup");
    let validator_three_address = eth::address_of(&validator_three_key)
        .expect("derive validator-3 address")
        .to_string();
    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.validator_status(port, &validator_three_address),
            Some(JAILED),
            "validator-3 is not deterministically JAILED on port {port}"
        );
        assert!(
            world
                .rpc
                .finalized(port)
                .is_some_and(|height| height > last_deadline),
            "later deadline stopped finality on port {port}"
        );
    }
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
    assert_eq!(
        state.forming_end - state.forming_start,
        50 * 3_600,
        "fresh process evidence must retain the canonical 50-hour FORMING duration"
    );
    assert_eq!(state.lookback_end, state.forming_end);
    assert_eq!(
        state.offering_end - state.lookback_end,
        48 * 3_600,
        "fresh process evidence must retain the canonical 48-hour OFFERING duration"
    );
    assert_eq!(
        state.scheduled_process_time - state.offering_end,
        12 * 3_600,
        "fresh process evidence must retain the canonical 12-hour WAITING duration"
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
    let target = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("fresh Metadosis creation evidence")
        .scheduled_process_time
        .saturating_add(1);
    advance_fresh_metadosis_time(world, target, &[(0, 1), (1, 2), (2, 3), (3, 4)], 8);
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

fn advance_fresh_metadosis_time(
    world: &mut World,
    requested_timestamp: u64,
    expected_edges: &[(u8, u8)],
    expected_persisted_status: u8,
) {
    let before_restart = finalized_points_at_common_height(world, 1);
    let before_height = before_restart[0].block_number;
    let offset = logical_time_offset(requested_timestamp, unix_time_secs());
    world
        .localnet
        .restart_committee_at_unix_time_offset(offset)
        .unwrap_or_else(|error| {
            panic!(
                "restart the complete committee at logical timestamp {requested_timestamp}: {error:#}"
            )
        });
    restart_ocomp_roles_after_committee_time_change(world);

    let worldwide_day = fresh_metadosis_wwd(world);
    let deadline = Instant::now() + Duration::from_secs(240);
    let (after_restart, changes) = loop {
        let points = finalized_points_at_common_height(world, before_height.saturating_add(1));
        let common_height = points[0].block_number;
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
        if states.iter().all(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.status == expected_persisted_status)
        }) && changes.iter().all(Option::is_some)
        {
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
                break (points, first);
            }
        }
        assert!(
            Instant::now() < deadline,
            "fresh Metadosis WWD did not reach status {expected_persisted_status} with edges \
             {expected_edges:?}; the production one-hour timestamp drift ratchet may be stalled"
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
    assert!(
        transition_timestamp >= requested_timestamp.saturating_sub(1),
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

fn restart_ocomp_roles_after_committee_time_change(world: &mut World) {
    for validator_index in 0..4_u8 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor after node restart: {error}")
            });
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} RPC exporter after node restart: {error}")
            });
        world
            .ocomp
            .restart_snapshot_exporter(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} RPC exporter: {error}")
            });
        world
            .ocomp
            .restart_supervisor(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Supervisor: {error}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP RPC-driven supervisors restart after logical-time change");
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
                let points = ports
                    .iter()
                    .enumerate()
                    .map(|(validator_index, port)| MetadosisFinalizedPointV1 {
                        validator_index: u8::try_from(validator_index)
                            .expect("validator index fits u8"),
                        block_number: common_height,
                        block_hash: world
                            .rpc
                            .block_hash(*port, common_height)
                            .and_then(|hash| B256::from_str(&hash).ok())
                            .expect("canonical finalized block hash"),
                        block_timestamp: world
                            .rpc
                            .block_timestamp(*port, common_height)
                            .expect("canonical finalized block timestamp"),
                    })
                    .collect::<Vec<_>>();
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
    let private_keys = world.state.ocomp_capacity_tribute_private_keys.clone();
    assert_eq!(
        private_keys.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "capacity fixture did not retain exactly 257 funded owners"
    );
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
                        rpc.tribute_offer(private_key, &worldwide_day)
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
                world.rpc.wait_successful_receipt(transaction_hash, 240),
                "capacity Tribute transaction did not succeed: {transaction_hash}"
            );
        }
        transaction_hashes.extend(batch);
    }

    assert_eq!(
        transaction_hashes.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "not every capacity owner submitted a public Tribute"
    );
    world.state.ocomp_capacity_tribute_tx_hashes = transaction_hashes;
}

#[then("all validators observe exactly 257 public Tributes for the capacity day")]
fn all_validators_observe_257_public_tributes(world: &mut World) {
    let transaction_hashes = &world.state.ocomp_capacity_tribute_tx_hashes;
    assert_eq!(
        transaction_hashes.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "capacity Tribute transaction set is incomplete"
    );
    for transaction_hash in transaction_hashes {
        assert!(
            world.rpc.wait_successful_receipt(transaction_hash, 240),
            "capacity Tribute transaction did not succeed: {transaction_hash}"
        );
    }

    let expected_supply = OCOMP_CAPACITY_TRIBUTE_COUNT.to_string();
    let primary = world.validators.primary_port();
    let supply_deadline = Instant::now() + Duration::from_secs(60);
    while world.rpc.supply(primary).as_deref() != Some(expected_supply.as_str()) {
        assert!(
            Instant::now() < supply_deadline,
            "capacity Tributes did not produce total supply {expected_supply}"
        );
        sleep(Duration::from_millis(250));
    }

    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("capacity WorldwideDay")
        .parse::<u32>()
        .expect("numeric capacity WorldwideDay");
    for port in world.validators.committee_ports() {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(ids) = world.rpc.tributes_by_day(port, worldwide_day) {
                let distinct = ids.iter().collect::<std::collections::BTreeSet<_>>();
                if ids.len() == OCOMP_CAPACITY_TRIBUTE_COUNT
                    && distinct.len() == OCOMP_CAPACITY_TRIBUTE_COUNT
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "validator on port {port} did not expose 257 distinct public Tributes"
            );
            sleep(Duration::from_millis(250));
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
            .mongodb
            .wait_for_tribute_projection(transaction_hash, 60)
            .unwrap_or_else(|error| {
                panic!(
                    "capacity boundary Tribute {transaction_hash} was not projected by every validator: {error}"
                )
            });
    }
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
    let deadline = Instant::now() + Duration::from_secs(OCOMP_FINAL_JOB_REQUEST_TIMEOUT_SECS);
    let activation_height = world
        .state
        .ocomp_activation_height
        .expect("prepared OCOMP activation height");
    let request = loop {
        let observed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_on(port, activation_height)
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

#[when("the validator supervisors submit results directly for that finalized JobIntent")]
fn validator_supervisors_submit_results_directly(world: &mut World) {
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
    world.state.ocomp_validator_balances_before = (0..4)
        .map(|validator_index| {
            let key = world
                .validators
                .get(validator_index)
                .evm_key()
                .expect("read validator EVM key");
            let address = eth::address_of(&key).expect("derive validator EVM address");
            let balance = world
                .rpc
                .balance_on(primary, &format!("{address:#x}"))
                .expect("read validator EVM balance before result votes");
            (address, balance)
        })
        .collect();
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("validator supervisors remain alive for direct ResultVote submission");
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
        let finalized_height = world.rpc.finalized(primary).unwrap_or_default();
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
    let validator = world.validators.get(VALIDATOR_INDEX);
    let key = validator
        .evm_key()
        .expect("read validator-2 EVM key for address derivation");
    let address = eth::address_of(&key).expect("derive validator-2 EVM address");
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
        world.rpc.head(primary).unwrap_or_default() < request.deadline_height,
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
    while world.rpc.head(primary).unwrap_or_default() < target_parent {
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
    quorum_applies_lysis_and_creates_nod_with_vote_count(world, 4);
}

#[then("three compatible validator domains atomically apply Lysis and create the Nod")]
fn compatible_quorum_applies_lysis_and_creates_nod(world: &mut World) {
    quorum_applies_lysis_and_creates_nod_with_vote_count(world, 3);
}

fn quorum_applies_lysis_and_creates_nod_with_vote_count(
    world: &mut World,
    expected_vote_count: usize,
) {
    assert!(
        matches!(expected_vote_count, 3 | 4),
        "PoC quorum scenario expects either three or four public votes"
    );
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
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
                        accountability.slot_validator_indexes.len() == expected_vote_count
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
                    "the expected {expected_vote_count} timely validator votes did not reach \
                     finalized accountability: {observed:?}"
                );
                sleep(Duration::from_millis(250));
            };
            assert_eq!(accountability.job_id, activation.job_id);
            let expected_validator_indexes = if expected_vote_count == 4 {
                vec![0, 1, 2, 3]
            } else {
                vec![1, 2, 3]
            };
            assert_eq!(
                accountability.slot_validator_indexes,
                expected_validator_indexes
            );
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
                expected_vote_count,
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
                expected_vote_count,
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
                    "validator {address:#x} paid for a ZeroFee result vote"
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

    let retry_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), vote_bytes)
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
    let mutation_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), mutated_bytes)
        .expect("submit changed-binding result vote through public RPC");
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
    world.state.ocomp_changed_completed_binding_reverted = Some(true);

    let retry_block = world
        .rpc
        .receipt_block_number(&retry_hash, primary)
        .expect("exact retry block");
    let mutation_block = world
        .rpc
        .receipt_block_number(&mutation_hash, primary)
        .expect("changed-binding block");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, retry_block.max(mutation_block), 60),
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
        assert_eq!(&accountability, expected_accountability);

        let generation = world
            .rpc
            .finalized_ocomp_certified_generation_on(port, activation)
            .expect("certified generation after public retry/mutation");
        assert_eq!(&generation, expected_generation);
    }
    world.state.ocomp_completed_state_unchanged = Some(true);
}

#[when("validators 2 and 3 OCOMP supervisors are stopped before the job")]
fn stop_two_supervisors_before_job(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = world.rpc.finalized(primary);
    for validator_index in [2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor before the job: {error}")
            });
    }
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
    let deadline = Instant::now() + Duration::from_secs(OCOMP_FINAL_JOB_REQUEST_TIMEOUT_SECS);

    let (request, record) = loop {
        let requests = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_on(port, activation_height)
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
    assert!(
        world
            .state
            .ocomp_finality_before_fault
            .zip(world.rpc.finalized(world.validators.primary_port()))
            .is_some_and(|(before, after)| after > before),
        "consensus finality did not advance with two OCOMP supervisors stopped"
    );
    world.state.ocomp_dynamic_job_requests = vec![request];
}

#[when("validators 1, 2 and 3 OCOMP supervisors are stopped before the job")]
fn stop_three_supervisors_before_job(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor before the job: {error}")
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
    mutated.job_id = B256::repeat_byte(0x5a);
    eprintln!("OCOMP_PUBLIC_MUTATION stage=submit_changed_binding");
    let mutation_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(
            primary,
            &world.validators.get(1),
            mutated
                .encode_canonical(&poc_schema_limits())
                .expect("canonical changed-binding payload"),
        )
        .expect("submit changed-binding public vote");
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
    world.state.ocomp_non_quorum_changed_binding_reverted = Some(true);
    let mutation_height = world
        .rpc
        .receipt_block_number(&mutation_hash, primary)
        .expect("changed-binding inclusion height");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=wait_changed_binding_finality inclusion_height={mutation_height}"
    );
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, mutation_height, 60),
        "changed-binding vote did not finalize"
    );
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

#[when("the three stopped supervisors restart and form the remaining quorum")]
fn restart_three_supervisors_for_quorum(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .restart_supervisor(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Supervisor for quorum: {error}")
            });
    }
}

#[then("the no-quorum job expires at its exclusive deadline without creating Nod")]
fn no_quorum_job_expires_without_nod(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized no-quorum JobIntent");
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(240);
    let records = loop {
        let finalized = world.rpc.finalized(primary).unwrap_or_default();
        let records = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if finalized >= request.deadline_height
            && records.iter().all(|record| {
                record
                    .as_ref()
                    .is_some_and(|record| record.status == OcompJobStatus::Expired)
            })
        {
            break records;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("remaining OCOMP roles stay alive during no-quorum expiry");
        assert!(
            Instant::now() < timeout,
            "no-quorum JobIntent did not expire at deadline {}: finalized={finalized}, \
             records={records:?}",
            request.deadline_height
        );
        sleep(Duration::from_millis(250));
    };
    let record = records[0].clone().expect("all expired records are present");
    assert!(
        records
            .iter()
            .all(|observed| observed.as_ref() == Some(&record)),
        "validators expose different expired JobIntent state"
    );
    let terminal = record.terminal.expect("expired terminal record");
    assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
    assert_eq!(terminal.terminal_height, request.deadline_height);
    assert!(terminal.completed_binding.is_none());
    let finalized = record.finalized.expect("finalized expired job");
    assert!(finalized.quorum.is_none());

    let accountability = world
        .rpc
        .finalized_ocomp_vote_accountability_on(primary, finalized.job_id)
        .expect("closed no-quorum accountability");
    assert_eq!(accountability.slot_validator_indexes, vec![0, 1]);
    assert_eq!(accountability.quorum_result_digest, None);
    assert_eq!(accountability.closed_height, Some(request.deadline_height));
    assert_eq!(accountability.timely_bitmap, Some(vec![0b0011]));
    assert_eq!(accountability.missing_bitmap, Some(vec![0b1100]));
    assert_eq!(accountability.equivocation_bitmap, Some(vec![0]));

    for port in world.validators.committee_ports() {
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
                .finalized_ocomp_activation_on(port, request.request_height, request.intent_id)
                .is_none(),
            "expired no-quorum job emitted a Lysis apply event on port {port}"
        );
    }
    assert!(
        world.rpc.finalized(primary).unwrap_or_default()
            > world.state.ocomp_finality_before_fault.unwrap_or_default(),
        "consensus finality did not advance while two Supervisors were stopped"
    );
    world.state.ocomp_vote_accountability = Some(accountability);
    world.state.ocomp_expired_without_nod = Some(true);
}

#[then("all four OCOMP domains run their node-facing production roles")]
fn four_domains_run_node_facing_roles(world: &mut World) {
    let records = world.ocomp.process_records();
    for validator_index in 0..4_u8 {
        for role in [
            OcompProcessRole::Supervisor,
            OcompProcessRole::SnapshotExporter,
        ] {
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
}

#[then("each OCOMP domain owns one authenticated production worker")]
fn four_domains_own_authenticated_workers(world: &mut World) {
    let records = world.ocomp.process_records();
    assert_eq!(
        records.len(),
        12,
        "expected one supervisor, one RPC exporter and one worker in each domain"
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
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized public Lysis activation");
    world
        .ocomp
        .verify_completed_job_artifacts(activation.job_id)
        .expect("verify every pinned validator's completed production job footprint");
}

#[when("validator 0 OCOMP supervisor is stopped through the typed fault control")]
fn stop_validator_zero_supervisor(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = world.rpc.finalized(primary);
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index: 0 })
        .expect("stop only validator-0 supervisor");
}

#[then("consensus finality advances while only that supervisor remains stopped")]
fn finality_advances_after_supervisor_stop(world: &mut World) {
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("height captured before OCOMP fault");
    let primary = world.validators.primary_port();
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, before.saturating_add(2), 60),
        "consensus finality did not advance after stopping an OCOMP supervisor"
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
    assert_eq!(stopped[0].role, OcompProcessRole::Supervisor);
}

#[then("validator 0 OCOMP supervisor restarts through the typed topology")]
fn validator_zero_supervisor_restarts(world: &mut World) {
    world
        .ocomp
        .restart_supervisor(0)
        .expect("restart only validator-0 OCOMP supervisor");
    let records = world.ocomp.process_records();
    let validator_zero_supervisors = records
        .iter()
        .filter(|record| {
            record.validator_index == Some(0)
                && record.role == OcompProcessRole::Supervisor
                && record.worker_ordinal.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validator_zero_supervisors.len(),
        2,
        "restart must retain the stopped lifecycle record and add one process"
    );
    assert_eq!(
        validator_zero_supervisors
            .iter()
            .filter(|record| record.stopped_at_millis.is_none())
            .count(),
        1,
        "exactly one validator-0 supervisor must be live after restart"
    );
}

#[when("validator 0 restarts before, across, and after the OCOMP fork height")]
fn validator_zero_restarts_across_fork(world: &mut World) {
    const VALIDATOR_INDEX: usize = 0;
    const ACTIVE_PROTOCOL_VERSION: u64 = 1;

    let activation = world
        .state
        .ocomp_activation_height
        .expect("prepared Final OCOMP activation height");
    let primary = world.validators.http_port(VALIDATOR_INDEX);
    let witness = world.validators.http_port(1);
    let pre_fork_restart_from_height = world.rpc.head(primary).expect("validator-0 head");
    assert!(
        pre_fork_restart_from_height + 4 < activation,
        "restart scenario started too close to H: head={pre_fork_restart_from_height}, H={activation}"
    );

    world
        .localnet
        .restart_validator_and_enclave(VALIDATOR_INDEX)
        .expect("restart validator-0 before H");
    let pre_fork_rejoined_height = world
        .rpc
        .wait_block(primary, pre_fork_restart_from_height.saturating_add(1), 20)
        .expect("validator-0 rejoins before H");
    assert!(
        pre_fork_rejoined_height < activation,
        "pre-fork restart rejoined after H: head={pre_fork_rejoined_height}, H={activation}"
    );

    let deadline = Instant::now() + Duration::from_secs(90);
    let down_across_fork_from_height = loop {
        let head = world.rpc.head(witness).expect("witness head before H");
        if head >= activation.saturating_sub(2) {
            assert!(
                head < activation,
                "missed the pre-H shutdown window: head={head}, H={activation}"
            );
            break head;
        }
        assert!(
            Instant::now() < deadline,
            "witness did not approach OCOMP activation height {activation}"
        );
        sleep(Duration::from_millis(100));
    };

    world
        .localnet
        .kill_validator(VALIDATOR_INDEX)
        .expect("keep validator-0 down across H");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(witness, activation.saturating_add(1), 60),
        "three live validators did not finalize through H while validator-0 was down"
    );
    let finalized_while_down_height = world.rpc.finalized(witness).expect("witness finality");

    world
        .localnet
        .restart()
        .expect("restart validator-0 after the network finalized H");
    let replayed_through_height = world
        .rpc
        .wait_block(primary, finalized_while_down_height, 60)
        .expect("validator-0 replays through the finalized activation block");
    assert_eq!(
        world.rpc.active_version_on(primary),
        Some(ACTIVE_PROTOCOL_VERSION),
        "restarted validator did not replay the protocol-v1 activation"
    );
    assert_eq!(
        world.rpc.active_version_on(witness),
        Some(ACTIVE_PROTOCOL_VERSION),
        "live committee did not activate protocol v1 at H"
    );

    let post_fork_restart_from_height = world.rpc.head(primary).expect("post-H validator-0 head");
    assert!(post_fork_restart_from_height >= activation);
    world
        .localnet
        .restart_validator_and_enclave(VALIDATOR_INDEX)
        .expect("restart validator-0 after H");
    let post_fork_rejoined_height = world
        .rpc
        .wait_block(primary, post_fork_restart_from_height.saturating_add(1), 30)
        .expect("validator-0 rejoins after H");
    assert_eq!(
        world.rpc.active_version_on(primary),
        Some(ACTIVE_PROTOCOL_VERSION),
        "post-H restart lost the activated protocol version"
    );
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("node-facing OCOMP roles survive validator restarts");
    world
        .ocomp
        .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
            validator_index: VALIDATOR_INDEX as u8,
            activation_height: activation,
            pre_fork_restart_from_height,
            pre_fork_rejoined_height,
            down_across_fork_from_height,
            finalized_while_down_height,
            replayed_through_height,
            post_fork_restart_from_height,
            post_fork_rejoined_height,
            active_protocol_version: ACTIVE_PROTOCOL_VERSION,
        })
        .expect("retain validated fork restart evidence");
}

#[then("the OCOMP evidence records successful H-1, H, and H+1 recovery")]
fn fork_restart_evidence_is_closed(world: &mut World) {
    let snapshot = world
        .ocomp
        .evidence_snapshot()
        .expect("build OCOMP restart evidence snapshot");
    snapshot
        .validate()
        .expect("validate OCOMP restart evidence");
    assert!(
        snapshot.fork_restart.is_some(),
        "restart scenario must retain exact fork-height observations"
    );
}

#[when("validator 0 restarts with a different valid immutable OCOMP fork install")]
fn validator_zero_restarts_with_mismatched_fork_install(world: &mut World) {
    const VALIDATOR_INDEX: usize = 0;
    const ACTIVE_PROTOCOL_VERSION: u64 = 1;

    let primary = world.validators.http_port(VALIDATOR_INDEX);
    let witness = world.validators.http_port(1);
    let activation = world
        .state
        .ocomp_activation_height
        .expect("prepared Final OCOMP activation height");
    let canonical_head_before_restart = world.rpc.head(primary).expect("validator-0 head");
    assert!(
        canonical_head_before_restart < activation,
        "fork mismatch must be installed before H"
    );

    let mismatched = world
        .ocomp
        .prepare_mismatched_fork_manifest(VALIDATOR_INDEX as u8)
        .expect("create a valid same-genesis manifest with a distinct install");
    world
        .localnet
        .restart_validator_with_chain_manifest(VALIDATOR_INDEX, mismatched.path.clone())
        .expect("restart validator-0 with its mismatched immutable fork install");

    assert!(
        world
            .rpc
            .wait_finalized_at_least(witness, activation.saturating_add(1), 60,),
        "the three canonical validators did not finalize through H"
    );
    let canonical_finalized_after_fork = world
        .rpc
        .finalized(witness)
        .expect("canonical finality after H");
    let mismatched_head_after_fork = world.rpc.head(primary).expect("mismatched validator head");
    assert!(
        mismatched_head_after_fork < activation,
        "mismatched validator imported the canonical activation block"
    );
    let canonical_active_protocol_version = world
        .rpc
        .active_version_on(witness)
        .expect("canonical active protocol version");
    let mismatched_active_protocol_version = world
        .rpc
        .active_version_on(primary)
        .expect("mismatched validator active protocol version");
    assert_eq!(
        canonical_active_protocol_version, ACTIVE_PROTOCOL_VERSION,
        "canonical committee did not activate protocol v1"
    );
    assert_eq!(
        mismatched_active_protocol_version, 0,
        "mismatched validator crossed the canonical Update boundary"
    );

    world
        .ocomp
        .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
            validator_index: VALIDATOR_INDEX as u8,
            canonical_install_hash: format!("{:#x}", mismatched.canonical_install_hash),
            mismatched_install_hash: format!("{:#x}", mismatched.mismatched_install_hash),
            canonical_activation_height: mismatched.canonical_activation_height,
            mismatched_activation_height: mismatched.mismatched_activation_height,
            canonical_head_before_restart,
            mismatched_head_after_fork,
            canonical_finalized_after_fork,
            canonical_active_protocol_version,
            mismatched_active_protocol_version,
        })
        .expect("retain validated fork mismatch evidence");
}

#[then("the canonical committee finalizes through H while the mismatched validator stays before H")]
fn fork_mismatch_is_fail_closed(world: &mut World) {
    let snapshot = world
        .ocomp
        .evidence_snapshot()
        .expect("build OCOMP fork mismatch evidence snapshot");
    snapshot
        .validate()
        .expect("validate OCOMP fork mismatch evidence");
    assert!(
        snapshot.fork_mismatch.is_some(),
        "fork mismatch scenario must retain exact install and height observations"
    );
}

#[when("validator 0 OCOMP supervisor is replaced by an incompatible peer")]
fn replace_validator_zero_with_incompatible_supervisor(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = world.rpc.finalized(primary);
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index: 0 })
        .expect("stop validator-0 canonical Supervisor");
    world
        .ocomp
        .restart_incompatible_supervisor(0)
        .expect("start validator-0 incompatible Supervisor");
}

#[then("the incompatible supervisor remains outside OCOMP while consensus finality advances")]
fn incompatible_supervisor_isolated_from_consensus(world: &mut World) {
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("height captured before incompatible Supervisor");
    let primary = world.validators.primary_port();
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, before.saturating_add(2), 60),
        "consensus finality did not advance with one incompatible Supervisor"
    );
    let log = world
        .ocomp
        .supervisor_log_tail(0, 80)
        .expect("read validator-0 Supervisor log");
    assert!(
        log.contains("OCOMP worker registration rejected: worker identity does not match the Supervisor domain"),
        "validator-0 Supervisor did not record the incompatible Worker registration:\n{log}"
    );
    let records = world.ocomp.process_records();
    let validator_zero_supervisors = records
        .iter()
        .filter(|record| {
            record.validator_index == Some(0)
                && record.role == OcompProcessRole::Supervisor
                && record.worker_ordinal.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validator_zero_supervisors.len(),
        2,
        "incompatible replacement must retain both lifecycle records"
    );
    assert_eq!(
        validator_zero_supervisors
            .iter()
            .filter(|record| record.stopped_at_millis.is_some())
            .count(),
        1,
        "the canonical validator-0 Supervisor was not stopped exactly once"
    );
    assert_eq!(
        validator_zero_supervisors
            .iter()
            .filter(|record| record.stopped_at_millis.is_none())
            .count(),
        1,
        "the incompatible validator-0 Supervisor is not the sole live replacement"
    );

    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("incompatible-Supervisor scenario JobIntent");
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("three compatible domains activated Lysis");
    let deadline = Instant::now() + Duration::from_secs(180);
    let closed = loop {
        let observed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_vote_accountability_on(port, activation.job_id)
            })
            .collect::<Vec<_>>();
        if observed.iter().all(|value| {
            value.as_ref().is_some_and(|accountability| {
                accountability.closed_height == Some(request.deadline_height)
            })
        }) {
            let first = observed[0]
                .clone()
                .expect("all closed accountability records are present");
            assert!(
                observed.iter().all(|value| value.as_ref() == Some(&first)),
                "validators expose different closed accountability after one incompatible domain"
            );
            break first;
        }
        assert!(
            Instant::now() < deadline,
            "response window did not close with validator 0 recorded missing: {observed:?}"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(closed.quorum_result_digest, Some(activation.result_digest));
    assert_eq!(closed.quorum_signer_bitmap, Some(vec![0b1110]));
    assert_eq!(closed.timely_bitmap, Some(vec![0b1110]));
    assert_eq!(closed.matching_bitmap, Some(vec![0b1110]));
    assert_eq!(closed.divergent_bitmap, Some(vec![0]));
    assert_eq!(closed.missing_bitmap, Some(vec![0b0001]));
    assert_eq!(closed.equivocation_bitmap, Some(vec![0]));
}

#[when("all validator nodes and OCOMP node-facing processes restart with preserved data")]
fn restart_completed_network_and_ocomp_processes(world: &mut World) {
    let primary = world.validators.primary_port();
    let before = world
        .rpc
        .finalized(primary)
        .expect("finality before restart");
    for validator_index in 0..4 {
        world
            .localnet
            .restart_validator_preserving_enclave(validator_index)
            .unwrap_or_else(|error| {
                panic!(
                    "restart validator-{validator_index} with its preserved datadir and enclave: {error:#}"
                )
            });
        let port = world.validators.http_port(validator_index);
        assert!(
            world
                .rpc
                .wait_block(port, before.saturating_add(1), 60)
                .is_some(),
            "validator-{validator_index} did not rejoin after preserved-datadir restart"
        );
    }

    for validator_index in 0..4_u8 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor for replay: {error}")
            });
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} RPC exporter for replay: {error}")
            });
        world
            .ocomp
            .restart_snapshot_exporter(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} RPC exporter: {error}")
            });
        world
            .ocomp
            .restart_supervisor(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Supervisor: {error}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all restarted OCOMP RPC-driven supervisors remain live");
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
    let replay_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), vote_bytes)
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
        .localnet
        .launch_follower("follower", OCOMP_TRACE_FOLLOWER_SLOT, 0, 0)
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
            .ocomp_runtime_trace_markers("follower")
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

/// The `submitLysisResult` selector is publicly callable on any chain. While
/// the OCOMP lifecycle is inactive it must behave as an ordinary reverting
/// call — included with a failed receipt, never aborting payload building
/// (the 2026-05-15 proposer-stall incident class).
#[when("an operator submits a Lysis result vote before OCOMP activation")]
fn operator_submits_lysis_vote_before_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let tx_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), vec![0_u8; 8])
        .expect("a pre-activation Lysis vote must be includable, not stall the proposer");
    let receipt = world
        .rpc
        .transaction_receipt(&tx_hash, primary)
        .expect("pre-activation Lysis vote receipt");
    assert_eq!(
        receipt.get("status").and_then(serde_json::Value::as_str),
        Some("0x0"),
        "a pre-activation Lysis vote must revert, not succeed"
    );
    let inclusion_block = world
        .rpc
        .receipt_block_number(&tx_hash, primary)
        .expect("pre-activation Lysis vote inclusion block");
    world.state.metadosis_inactive_lysis_vote_hash = Some(tx_hash);
    world.state.metadosis_inactive_lysis_vote_block = Some(inclusion_block);
}

#[then("the vote transaction reverts with the lifecycle-inactive code and finalization continues")]
fn lysis_vote_reverts_with_lifecycle_inactive_code(world: &mut World) {
    let primary = world.validators.primary_port();
    let inclusion_block = world
        .state
        .metadosis_inactive_lysis_vote_block
        .expect("pre-activation Lysis vote inclusion block");

    // Receipts omit revert data; replay the identical calldata through
    // `eth_call` to observe the machine-readable rejection bytes.
    let calldata = {
        use alloy_sol_types::SolCall as _;
        crate::internal::eth::IMetadosis::submitLysisResultCall {
            resultVoteV1: alloy_primitives::Bytes::from(vec![0_u8; 8]),
        }
        .abi_encode()
    };
    let revert_data = eth::call_revert_data(
        &world.rpc.url(primary),
        crate::internal::addresses::WWD_ADDR,
        calldata,
    )
    .expect("pre-activation Lysis vote must revert with rejection data");
    let mut expected = Vec::with_capacity(36);
    expected.extend_from_slice(
        outbe_ocomp_protocol::abi::OCOMP_RESULT_VOTE_REJECTED_SELECTOR.as_slice(),
    );
    expected.extend_from_slice(&alloy_primitives::U256::from(5_u64).to_be_bytes::<32>());
    assert_eq!(
        revert_data, expected,
        "pre-activation rejection must be OcompResultVoteRejected(5)"
    );
    world.state.metadosis_inactive_lysis_reject_code = Some(5);

    // The chain must keep finalizing past the reverting transaction on every
    // validator: the selector must never poison block production.
    for port in world.validators.committee_ports() {
        assert!(
            world
                .rpc
                .wait_finalized_at_least(port, inclusion_block + 2, 60),
            "finalization stalled after a pre-activation Lysis vote on port {port}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::joiner_restart_is_safely_after_boundary;

    #[test]
    fn joiner_restart_waits_out_an_imminent_dkg_activation() {
        assert!(!joiner_restart_is_safely_after_boundary(78, 78, 20));
        assert!(!joiner_restart_is_safely_after_boundary(80, 80, 20));
        assert!(joiner_restart_is_safely_after_boundary(81, 81, 20));
        assert!(joiner_restart_is_safely_after_boundary(82, 82, 20));
        assert!(!joiner_restart_is_safely_after_boundary(83, 83, 20));
    }

    #[test]
    fn joiner_restart_window_derives_from_the_chain_epoch() {
        assert!(!joiner_restart_is_safely_after_boundary(118, 118, 120));
        assert!(joiner_restart_is_safely_after_boundary(121, 121, 120));
        assert!(joiner_restart_is_safely_after_boundary(122, 122, 120));
    }

    #[test]
    fn joiner_restart_requires_the_full_node_to_finalize_the_boundary_block() {
        assert!(!joiner_restart_is_safely_after_boundary(101, 100, 20));
        assert!(joiner_restart_is_safely_after_boundary(101, 101, 20));
    }
}
