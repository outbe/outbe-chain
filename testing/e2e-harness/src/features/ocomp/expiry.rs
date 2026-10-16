use crate::features::ocomp::*;
use outbe_primitives::time::SECONDS_PER_DAY;

mod late_result;
mod votes;

#[cfg(test)]
pub(super) use late_result::late_result_expected_accountability;

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
    // move this boundary. Its independence from workers is the property tested.
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
            &crate::world::ocomp::OutageExportJob {
                record: &record,
                checkpoint,
                bundle: &bundle,
            },
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
    assert_job_expires_without_nod(
        world,
        ExpiredVotes {
            voters: &[0, 1],
            timely_bitmap: 0b0011,
            missing_bitmap: 0b1100,
        },
        true,
        "two-vote",
    );
}

#[then("the unexported zero-vote job expires and reaches Released without an ACK")]
fn unexported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(
        world,
        ExpiredVotes {
            voters: &[],
            timely_bitmap: 0,
            missing_bitmap: 0b1111,
        },
        false,
        "zero-vote unexported",
    );
}

#[then("the exported zero-vote job expires at its exclusive deadline and finality continues")]
fn exported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(
        world,
        ExpiredVotes {
            voters: &[],
            timely_bitmap: 0,
            missing_bitmap: 0b1111,
        },
        true,
        "zero-vote exported",
    );
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
            .checked_add(SECONDS_PER_DAY)
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
    crate::features::l2_registration::ensure_tribute_offer_operator(world, &offerer);
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
