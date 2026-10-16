use crate::features::ocomp::*;

mod deadlines;

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

pub(in crate::features::ocomp) fn joiner_restart_is_in_safe_early_epoch_window(
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

type PublicVotes = crate::world::rpc::OcompPublicVoteAccountabilityV1;

struct DynamicJobPair {
    job_b_request: crate::world::rpc::OcompPublicJobRequestV1,
    job_a_record: OcompJobRecordV1,
    job_b_record: OcompJobRecordV1,
    job_a_votes: PublicVotes,
    job_b_votes: PublicVotes,
    joiner_participant_index: u16,
}

struct JobPairWatch<'a> {
    ports: Vec<u16>,
    job_a_request: &'a crate::world::rpc::OcompPublicJobRequestV1,
    job_b_wwd: u32,
    certified_five_member_height: u64,
}

/// The value every peer reports. The caller has checked that each peer reported one.
fn unanimous<T: Clone + PartialEq>(observed: &[Option<T>], disagreement: &str) -> T {
    let first = observed[0].clone().expect("every peer reported a value");
    assert!(
        observed
            .iter()
            .all(|candidate| candidate.as_ref() == Some(&first)),
        "{disagreement}"
    );
    first
}

fn all_voting_open(records: &[Option<OcompJobRecordV1>]) -> bool {
    records.iter().all(|record| {
        record
            .as_ref()
            .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
    })
}

fn finalized_records_on(
    world: &World,
    ports: &[u16],
    intent_id: B256,
) -> Vec<Option<OcompJobRecordV1>> {
    ports
        .iter()
        .map(|&port| world.rpc.finalized_ocomp_job_record_on(port, intent_id))
        .collect()
}

fn finalized_votes_on(world: &World, ports: &[u16], job_id: B256) -> Vec<Option<PublicVotes>> {
    ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(port, job_id)
        })
        .collect()
}

fn record_statuses(records: &[Option<OcompJobRecordV1>]) -> Vec<Option<OcompJobStatus>> {
    records
        .iter()
        .map(|record| record.as_ref().map(|record| record.status))
        .collect()
}

fn vote_slots(votes: &[Option<PublicVotes>]) -> Vec<Option<Vec<u16>>> {
    votes
        .iter()
        .map(|votes| {
            votes
                .as_ref()
                .map(|votes| votes.slot_validator_indexes.clone())
        })
        .collect()
}

fn finalized_job_id(record: &OcompJobRecordV1, what: &str) -> B256 {
    record
        .finalized
        .as_ref()
        .unwrap_or_else(|| panic!("{what} finalized intent"))
        .job_id
}

fn joiner_vote_slot(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
    job_b_id: B256,
    job_b_votes: &PublicVotes,
) -> Option<u16> {
    let finalized_height = world
        .rpc
        .finalized_result(world.validators.primary_port())
        .expect("observe finalized height for job B accountability");
    let joiner_vote = finalized_vote_for_delegate_on_job(
        world,
        request.request_height,
        finalized_height,
        world.validators.joiner_index(),
        job_b_id,
    )?;
    accountability_slot_for_vote(job_b_votes, &joiner_vote)
}

fn finalized_job_b_request(
    world: &World,
    watch: &JobPairWatch<'_>,
) -> Option<crate::world::rpc::OcompPublicJobRequestV1> {
    let requests = watch
        .ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_ocomp_job_request_for_worldwide_day_on(
                    port,
                    watch.job_a_request.request_height + 1,
                    watch.job_b_wwd,
                )
                .unwrap_or_else(|error| panic!("observe job B on port {port}: {error:#}"))
        })
        .collect::<Vec<_>>();
    if !requests.iter().all(Option::is_some) {
        return None;
    }
    let request = unanimous(
        &requests,
        "validators expose different finalized job B requests",
    );
    if request.worldwide_day != watch.job_b_wwd {
        return None;
    }
    assert!(
        request.request_height > watch.certified_five_member_height,
        "Job B request at height {} predates the certified five-member barrier at height {}; the harness released it too early",
        request.request_height,
        watch.certified_five_member_height
    );
    Some(request)
}

/// One poll of job B; `None` keeps waiting and updates `last_observation`.
fn observe_dynamic_job_pair(
    world: &World,
    watch: &JobPairWatch<'_>,
    last_observation: &mut String,
) -> Option<DynamicJobPair> {
    let request = finalized_job_b_request(world, watch)?;
    let job_a_records = finalized_records_on(world, &watch.ports, watch.job_a_request.intent_id);
    let job_b_records = finalized_records_on(world, &watch.ports, request.intent_id);
    let statuses = format!(
        "job_b_request=({:#x}, {}), job_a_statuses={:?}, job_b_statuses={:?}",
        request.intent_id,
        request.worldwide_day,
        record_statuses(&job_a_records),
        record_statuses(&job_b_records)
    );
    last_observation.clone_from(&statuses);
    if !(all_voting_open(&job_a_records) && all_voting_open(&job_b_records)) {
        return None;
    }
    let job_a_record = unanimous(
        &job_a_records,
        "validators expose different canonical job A records after activation",
    );
    let job_b_record = unanimous(
        &job_b_records,
        "validators expose different canonical job B records",
    );
    let job_b_id = finalized_job_id(&job_b_record, "job B");
    let job_a_accountability = finalized_votes_on(
        world,
        &watch.ports,
        finalized_job_id(&job_a_record, "job A"),
    );
    let job_b_accountability = finalized_votes_on(world, &watch.ports, job_b_id);
    *last_observation = format!(
        "{statuses}, job_a_slots={:?}, job_b_slots={:?}",
        vote_slots(&job_a_accountability),
        vote_slots(&job_b_accountability)
    );
    if !(job_a_accountability.iter().all(Option::is_some)
        && job_b_accountability.iter().all(Option::is_some))
    {
        return None;
    }
    let job_a_votes = unanimous(
        &job_a_accountability,
        "validators expose different job A accountability",
    );
    let job_b_votes = unanimous(
        &job_b_accountability,
        "validators expose different job B accountability",
    );
    let joiner_participant_index = joiner_vote_slot(world, &request, job_b_id, &job_b_votes)?;
    dynamic_pre_restart_vote_baseline_ready(
        job_a_votes.slot_validator_indexes.len(),
        job_b_votes.slot_validator_indexes.len(),
        true,
    )
    .then_some(DynamicJobPair {
        job_b_request: request,
        job_a_record,
        job_b_record,
        job_a_votes,
        job_b_votes,
        joiner_participant_index,
    })
}

fn assert_dynamic_job_snapshots(world: &World, pair: &DynamicJobPair, job_b_wwd: u32) {
    let DynamicJobPair {
        job_a_record,
        job_b_record,
        job_a_votes,
        job_b_votes,
        joiner_participant_index,
        ..
    } = pair;
    assert_eq!(job_a_record.intent.result_member_count, 4);
    assert_eq!(job_a_record.intent.result_quorum_threshold, 3);
    assert_eq!(job_a_votes.member_count, 4);
    assert_eq!(job_a_votes.quorum_threshold, 3);
    let job_a_id = finalized_job_id(job_a_record, "job A");
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
            .contains(joiner_participant_index),
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
    let watch = JobPairWatch {
        ports,
        job_a_request: &job_a_request,
        job_b_wwd,
        certified_five_member_height,
    };
    let deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let mut last_observation = "no finalized job B request observed".to_owned();
    let pair = loop {
        if let Some(pair) = observe_dynamic_job_pair(world, &watch, &mut last_observation) {
            break pair;
        }
        assert!(
            Instant::now() < deadline,
            "job B did not open and accept the fifth validator's vote before timeout: \
             {last_observation}"
        );
        sleep(Duration::from_millis(500));
    };
    assert_dynamic_job_snapshots(world, &pair, job_b_wwd);
    world
        .state
        .ocomp_dynamic_job_requests
        .push(pair.job_b_request);
}

fn advance_dynamic_membership_to_next_daily_cycle(world: &mut World) {
    use outbe_primitives::time::SECONDS_PER_DAY;

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
    crate::features::l2_registration::ensure_tribute_offer_operator(world, &key);
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
