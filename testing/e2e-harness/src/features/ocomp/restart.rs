use crate::features::ocomp::*;

const OCOMP_TRACE_FOLLOWER_SLOT: usize = 14;

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
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);

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
        world.rpc.wait_block(port, before, 60)
            .unwrap_or_else(|error| panic!("validator-{validator_index} did not restore its preserved finalized head: {error:#}"));
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
    if let Some(pending) =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication)
    {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            sleep(Duration::from_millis(250));
        }
    }
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
    assert_completed_replay_window_open(world);
    let replay_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &delegate_key, vote_bytes)
        .expect("submit exact full-result replay after restart");
    let replay_receipt = world
        .rpc
        .transaction_receipt(&replay_hash, primary)
        .expect("exact post-restart replay receipt");
    observe_timely_completed_replay(world, "after-restart", &replay_hash, &replay_receipt);
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
