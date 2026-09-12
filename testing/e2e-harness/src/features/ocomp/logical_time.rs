use crate::features::ocomp::*;

// Exact WorldwideDay VWAP formation always spans the canonical 50-hour window.
// The scenario advances that interval with the controlled logical-time ratchet;
// it must never shorten the consensus constant merely to make the E2E faster.
pub(in crate::features::ocomp) const METADOSIS_FRESH_FORMING_SECONDS: u64 =
    outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS;

pub(in crate::features::ocomp) fn dynamic_oracle_refresh_timestamp(next_daily_cycle: u64) -> u64 {
    const REFRESH_HEADROOM_SECS: u64 = 3 * 60 * 60;
    next_daily_cycle
        .checked_sub(REFRESH_HEADROOM_SECS)
        .expect("daily Cycle leaves three hours for an Oracle refresh")
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

pub(in crate::features::ocomp) fn first_protocol_cycle_at_or_after(
    world: &World,
    timestamp: u64,
) -> u64 {
    let genesis_path = world.ocomp.canonical_chain_manifest_path();
    let genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).expect("read ProtocolCycle genesis"))
            .expect("decode ProtocolCycle genesis");
    let interval = GenesisProtocolParametersV1::from_genesis(&genesis)
        .expect("read immutable ProtocolCycle interval")
        .metadosis_advance_interval_seconds;
    first_protocol_cycle_at_or_after_interval(timestamp, interval)
}

pub(in crate::features::ocomp) fn first_protocol_cycle_at_or_after_interval(
    timestamp: u64,
    interval: u64,
) -> u64 {
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
pub(in crate::features::ocomp) const RATCHET_STALL_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::features::ocomp) struct RestartBarrierState {
    pub(in crate::features::ocomp) lifecycle_observed: bool,
    pub(in crate::features::ocomp) publication_observed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::features::ocomp) enum RestartBarrierDecision {
    Continue(RestartBarrierState),
    Complete,
    HistoricalLifecycleRequired(RestartBarrierState),
}

pub(in crate::features::ocomp) fn restart_barrier_decision(
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

pub(in crate::features::ocomp) fn historical_lifecycle_scan_heights(
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

pub(in crate::features::ocomp) fn fresh_wwd_lifecycle_overshot(
    expected_status: u8,
    observed: &[Option<u8>],
) -> bool {
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

pub(in crate::features::ocomp) fn stop_ocomp_roles_before_committee_time_change(
    world: &mut World,
) -> OcompNodeFacingResumePlan {
    world
        .ocomp
        .suspend_node_facing_roles()
        .expect("suspend the exact OCOMP client inventory before node restart")
}

pub(in crate::features::ocomp) fn restart_ocomp_roles_after_committee_time_change(
    world: &mut World,
    plan: OcompNodeFacingResumePlan,
) {
    world
        .ocomp
        .resume_node_facing_roles(plan)
        .expect("restore the exact OCOMP client inventory after node restart");
}

pub(in crate::features::ocomp) fn post_restart_convergence_target(
    finalized_heights: impl IntoIterator<Item = u64>,
) -> u64 {
    finalized_heights
        .into_iter()
        .max()
        .expect("restarted validator cohort is non-empty")
        .checked_add(1)
        .expect("post-restart convergence height does not overflow")
}

pub(in crate::features::ocomp) fn fresh_metadosis_wwd(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("fresh Metadosis WorldwideDay")
        .parse::<u32>()
        .expect("numeric fresh Metadosis WorldwideDay")
}

pub(in crate::features::ocomp) fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs()
}

pub(in crate::features::ocomp) fn logical_time_offset(
    target_timestamp: u64,
    now_timestamp: u64,
) -> i64 {
    i64::try_from(i128::from(target_timestamp) - i128::from(now_timestamp))
        .expect("testnet logical time offset fits i64")
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
