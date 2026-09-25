use crate::features::ocomp::*;

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
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
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
        world.rpc.wait_block(port, before_restart, 60)
            .unwrap_or_else(|error| panic!("validator-{validator_index} did not restore finality after successor preload: {error:#}"));
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
    if let Some(pending) =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication)
    {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            sleep(Duration::from_millis(250));
        }
    }
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
    let propose_tx = world
        .rpc
        .send_propose(&proposer, &format!("{UPDATE_ADDR:#x}"), &payload)
        .expect("submit OCOMP successor proposal");
    assert!(
        world.rpc.wait_successful_receipt(&propose_tx, 40),
        "OCOMP successor proposal transaction failed: {propose_tx}"
    );
    let proposal_id = world
        .rpc
        .proposal_id_from_receipt(world.validators.primary_port(), &propose_tx)
        .expect("read the allocated OCOMP successor proposal id");
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
        world.rpc.wait_block(port, activation_height, 60)
            .unwrap_or_else(|error| panic!("validator-{validator_index} did not finalize the OCOMP activation height: {error:#}"));
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

pub(in crate::features::ocomp) fn assert_pending_v1_at_common_finality(
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
    complete_fresh_v2_tribute(world, successor_wwd.value(), 1);
}

#[when(expr = "validator {int} completes a fresh real-ZKP Tribute after the hardware upgrade")]
fn post_hardware_upgrade_tribute(world: &mut World, owner_index: usize) {
    assert!(
        matches!(owner_index, 2 | 3),
        "each post-upgrade owner must be unused"
    );
    let primary = world.validators.primary_port();
    let now = world
        .rpc
        .latest_block_timestamp(primary)
        .expect("post-upgrade chain clock");
    let today = WorldwideDay::from_timestamp(now).start_timestamp();
    let day = (0..=3)
        .find_map(|offset| {
            let day = WorldwideDay::from_timestamp(today + offset * 86_400).value();
            let state = world.rpc.metadosis_wwd_state_on(primary, day)?;
            (state.status <= 1
                && state.lookback_end > now
                && state.scheduled_process_time > state.lookback_end)
                .then_some(day)
        })
        .expect("a canonically created future WorldwideDay must have an unused OFFERING window");
    complete_fresh_v2_tribute(world, day, owner_index);
}

fn complete_fresh_v2_tribute(world: &mut World, successor_wwd_value: u32, owner_index: usize) {
    let ports = world.validators.committee_ports();
    let successor_bundle_hash = world
        .state
        .ocomp_successor_bundle_hash
        .expect("active V2 worker lane");
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
        .get(owner_index)
        .evm_key()
        .expect("fresh Tribute owner key");
    let tribute_tx = world
        .rpc
        .tribute_offer_for_network_with_params(
            &offerer,
            crate::internal::l2_fixture::FIXTURE_L2_CHAIN_ID,
            &successor_wwd_value.to_string(),
            "100",
            "0",
            840,
            false,
        )
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
        request.job_id,
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
