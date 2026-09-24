use crate::features::ocomp::*;

// A one-Tribute scenario can reach request publication well before its
// genesis-bound offering window closes, so the bounded wait includes the
// remaining phase interval plus finalization/request publication slack.
pub(in crate::features::ocomp) const OCOMP_JOB_REQUEST_TIMEOUT_SECS: u64 = 300;

#[then("Metadosis creates one finalized JobIntent from that public Tribute")]
pub(in crate::features::ocomp) fn metadosis_creates_finalized_job_intent(world: &mut World) {
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
        request.intent_id,
        Duration::from_secs(600),
    )
    .expect("arm exact V1 incarnations before releasing workers");
    world
        .ocomp
        .restart_worker_cohort(&[(0, 0), (1, 0), (2, 0), (3, 0)])
        .expect("release all held V1 workers before waiting for individual readiness");
    world.state.ocomp_pending_v1_workers_held = false;
}

pub(in crate::features::ocomp) fn case_one_compute_started_line(
    text: &str,
    job_id: B256,
) -> Option<&str> {
    let expected = format!("embedded OCOMP computation started job_id={job_id:#x}");
    text.lines().find(|line| {
        line.split_once(" INFO ")
            .and_then(|(_, message)| message.split_once("outbe_chain::ocomp_exex::compute: "))
            .is_some_and(|(_, message)| message.trim_end() == expected)
    })
}

/// Keep workers held until every current node has consumed its exact export
/// ACK and dispatched computation. Completed is not a substitute: a validator
/// first observing Completed need not start its own computation.
pub(in crate::features::ocomp) fn wait_case_one_worker_release(
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

#[then("three matching validator domains atomically apply Lysis and create the Nod")]
pub(in crate::features::ocomp) fn quorum_applies_lysis_and_creates_nod(world: &mut World) {
    quorum_applies_lysis_and_creates_nod_with_vote_expectation(
        world,
        PublicVoteSetExpectation::AnyQuorum,
    );
}

#[then("Lysis and OCOMP use the independently frozen previous-day entry price")]
fn lysis_and_ocomp_use_frozen_entry_price(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized public JobIntent");
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod generation");
    let actions = result_nod_actions_on(world, 0, generation.job_id);
    let [action] = actions.as_slice() else {
        panic!("single-Tribute pricing scenario must produce exactly one Nod action")
    };
    let wwd_vwap = world
        .rpc
        .ocomp_job_record_at_on(
            world.validators.primary_port(),
            request.intent_id,
            request.request_height,
        )
        .expect("read request-time WWD price")
        .intent
        .frozen_metadosis_values
        .current_vwap;
    let expected = crate::features::oracle_expectations::frozen_entry_price(world);
    assert_ne!(
        expected, wwd_vwap,
        "fixture distinguishes entry price from WWD VWAP"
    );
    assert_eq!(
        action.entry_price_minor, expected,
        "Lysis/Nod must carry the independently recomputed frozen entry price"
    );
}

#[then("three compatible validator domains atomically apply Lysis and create the Nod")]
fn compatible_quorum_applies_lysis_and_creates_nod(world: &mut World) {
    quorum_applies_lysis_and_creates_nod_with_vote_expectation(
        world,
        PublicVoteSetExpectation::Exact(&[1, 2, 3]),
    );
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

    assert_completed_replay_window_open(world);
    let retry_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &delegate_key, vote_bytes)
        .expect("submit exact completed-vote retry through public RPC");
    let retry_receipt = world
        .rpc
        .transaction_receipt(&retry_hash, primary)
        .expect("exact retry receipt");
    observe_timely_completed_replay(world, "after-retirement", &retry_hash, &retry_receipt);
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
    wait_for_common_finalized_checkpoint(world, finality_target, "public retry/mutation receipts");
}

pub(in crate::features::ocomp) fn assert_completed_replay_window_open(world: &World) {
    let request = world.state.ocomp_job_request.as_ref().expect("replay job");
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("head before replay");
    assert!(
        head.checked_add(1).is_some_and(|next| next < request.deadline_height),
        "positive replay has no admissible next block: head={head}, deadline={}; fix the scenario genesis window",
        request.deadline_height
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
    job_id: B256,
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
        .arm_completed_artifact_phase(bundle_hash, job_id, pids, budget)
}
