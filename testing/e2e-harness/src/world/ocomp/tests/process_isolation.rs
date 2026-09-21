use super::*;

#[test]
fn worker_outage_stop_evidence_must_match_actual_retained_ownership() {
    let (mut topology, mut evidence) = stopped_outage_fixture();
    topology.ensure_worker_cohort_stopped(&evidence).unwrap();
    evidence.stops[0].pid += 10;
    assert!(topology.ensure_worker_cohort_stopped(&evidence).is_err());
    evidence.stops[0].pid -= 10;
    topology.faults.pop();
    assert!(topology.ensure_worker_cohort_stopped(&evidence).is_err());
}

#[test]
fn worker_outage_rejects_reintroduced_worker_even_if_it_was_stopped_again() {
    let (mut topology, evidence) = stopped_outage_fixture();
    topology.ensure_worker_cohort_stopped(&evidence).unwrap();
    let mut replacement = topology.records[0].clone();
    replacement.pid += 10;
    replacement.started_at_millis = 30;
    replacement.stopped_at_millis = Some(40);
    topology.records.push(replacement);
    assert!(topology.ensure_worker_cohort_stopped(&evidence).is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn keyless_full_node_mismatch_marker_is_empty_and_one_shot() {
    let mut topology = topology_with_validators(4);
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    topology.launch_identity = Some(prepared.launch_identity());
    topology.stage_keyless_full_node_domain(4).unwrap();

    let marker = topology.arm_keyless_full_node_result_mismatch(4).unwrap();
    assert_eq!(fs::read(&marker).unwrap(), Vec::<u8>::new());
    assert!(topology.arm_keyless_full_node_result_mismatch(4).is_err());
    assert_eq!(
        topology.keyless_full_node_fatal_evidence_root(4).unwrap(),
        topology
            .cfg
            .validator_dir(4)
            .join("ocomp/domain-v1/node-v1/fatal-evidence")
    );
}

#[test]
fn typed_fault_stops_only_the_selected_owned_process() {
    if std::env::var_os(CHILD_MODE).is_some() {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }

    let mut topology = topology();
    topology
        .attach_owned(
            Some(0),
            OcompProcessRole::SnapshotExporter,
            None,
            child_guard(),
        )
        .unwrap();
    topology
        .attach_owned(
            Some(1),
            OcompProcessRole::SnapshotExporter,
            None,
            child_guard(),
        )
        .unwrap();

    topology
        .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index: 0 })
        .unwrap();

    assert!(topology.records[0].stopped_at_millis.is_some());
    assert!(topology.records[1].stopped_at_millis.is_none());
    assert!(topology.domains[0].snapshot_exporter.is_none());
    assert!(topology.domains[1].snapshot_exporter.is_some());
    assert_eq!(
        topology.faults,
        vec![OcompFaultRecordV1 {
            fault: OcompProcessFault::StopSnapshotExporter { validator_index: 0 },
            applied_at_millis: topology.faults[0].applied_at_millis,
        }]
    );

    let snapshot = topology.evidence_snapshot().unwrap();
    assert_eq!(snapshot.processes, topology.records);
    assert_eq!(snapshot.faults, topology.faults);
    assert_eq!(snapshot.domain_roots.len(), 4);
    let canonical = serde_json::to_vec(&snapshot).unwrap();
    assert_eq!(
        serde_json::from_slice::<OcompScenarioTopologyV1>(&canonical).unwrap(),
        snapshot
    );
}
