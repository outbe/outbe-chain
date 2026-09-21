use super::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn stopped_history_above_sixty_four_preserves_owned_attachment_and_audit_indices() {
    for keyless in [false, true] {
        for role in [OcompProcessRole::SnapshotExporter, OcompProcessRole::Worker] {
            let mut topology = completed_job_topology();
            let index = if keyless { 4 } else { 0 };
            if keyless {
                let root = topology.cfg.validator_dir(4).join("ocomp/domain-v1");
                topology.keyless_full_node_domain = Some((4, OcompDomain::new(root)));
            }
            // Retained audit rows, not 65 running children. Only the new
            // attachment and duplicate attempt below spawn owned children.
            topology.records = (1..=65)
                .map(|pid| OcompProcessRecordV1 {
                    validator_index: Some(index),
                    role: OcompProcessRole::Worker,
                    worker_ordinal: Some(0),
                    pid,
                    started_at_millis: 1,
                    stopped_at_millis: Some(2),
                })
                .collect();
            let ordinal = (role == OcompProcessRole::Worker).then_some(0);
            let guard = child_guard();
            let pid = guard.pid();
            if keyless {
                topology
                    .attach_keyless_full_node_owned(index, role, ordinal, guard)
                    .unwrap();
                assert!(topology
                    .attach_keyless_full_node_owned(index, role, ordinal, child_guard())
                    .is_err());
            } else {
                topology
                    .attach_owned(Some(index), role, ordinal, guard)
                    .unwrap();
                assert!(topology
                    .attach_owned(Some(index), role, ordinal, child_guard())
                    .is_err());
            }
            assert_eq!(topology.records.len(), 66);
            let process = {
                let domain = topology.compute_domain_mut(index).unwrap();
                match role {
                    OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.take().unwrap(),
                    OcompProcessRole::Worker => domain.workers.remove(&0).unwrap(),
                }
            };
            assert_eq!(process.record_index, 65);
            assert_eq!(process.guard.pid(), pid);
            topology.stop_owned(process);
            let evidence = topology.evidence_snapshot().unwrap();
            assert_eq!(evidence.processes.len(), 66);
            for (offset, record) in evidence.processes[..65].iter().enumerate() {
                assert_eq!(record.pid, u32::try_from(offset + 1).unwrap());
                assert_eq!(record.stopped_at_millis, Some(2));
            }
            assert_eq!(evidence.processes[65].pid, pid);
            assert_eq!(evidence.processes[65].role, role);
            assert!(evidence.processes[65].stopped_at_millis.is_some());
        }
    }
}

#[test]
fn topology_follows_the_configured_validator_count() {
    let topology = topology_with_validators(5);
    let roots = (0..5_u8)
        .map(|index| topology.domain_root(index).unwrap().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(roots.len(), 5);
    assert_eq!(
        roots
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5
    );
    assert!(topology.domain_root(5).is_err());
    assert!(topology.process_records().is_empty());
}

#[test]
fn active_joiner_appends_one_domain_without_rewriting_existing_roots() {
    let mut topology = topology_with_validators(4);
    let original = topology
        .validator_indices()
        .unwrap()
        .into_iter()
        .map(|index| topology.domain_root(index).unwrap().to_owned())
        .collect::<Vec<_>>();

    topology.add_active_validator_domain(4).unwrap();

    assert_eq!(topology.evidence_snapshot().unwrap().domain_roots.len(), 5);
    for (index, expected) in original.iter().enumerate() {
        assert_eq!(
            topology.domain_root(u8::try_from(index).unwrap()).unwrap(),
            expected
        );
    }
    assert!(topology.add_active_validator_domain(4).is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn keyless_workers_keep_exact_ordinals_outside_validator_membership() {
    let mut topology = completed_job_topology();
    let root = topology.cfg.validator_dir(4).join("ocomp/domain-v1");
    topology.keyless_full_node_domain = Some((4, OcompDomain::new(root)));
    for ordinal in [0, 1] {
        topology
            .attach_keyless_full_node_owned(
                4,
                OcompProcessRole::Worker,
                Some(ordinal),
                child_guard(),
            )
            .unwrap();
        topology.ensure_worker_alive(4, ordinal).unwrap();
    }
    assert_eq!(topology.validator_indices().unwrap(), [0, 1, 2, 3]);
    assert_eq!(
        topology
            .keyless_full_node_domain(4)
            .unwrap()
            .workers
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [0, 1],
    );
    assert_eq!(
        topology
            .records
            .iter()
            .map(|record| record.worker_ordinal)
            .collect::<Vec<_>>(),
        [Some(0), Some(1)],
    );
    assert!(topology
        .attach_keyless_full_node_owned(4, OcompProcessRole::Worker, Some(1), child_guard(),)
        .is_err());
    assert!(topology
        .attach_keyless_full_node_owned(4, OcompProcessRole::Worker, None, child_guard(),)
        .is_err());
    assert!(topology
        .attach_keyless_full_node_owned(4, OcompProcessRole::Worker, Some(2), child_guard(),)
        .is_err());
    assert!(topology.ensure_worker_alive(4, 2).is_err());
    assert!(topology.ensure_worker_alive(5, 0).is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn keyless_full_node_profile_is_complete_without_joining_active_topology() {
    let mut topology = topology_with_validators(4);
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    topology.launch_identity = Some(prepared.launch_identity());

    let args = topology.stage_keyless_full_node_domain(4).unwrap();

    assert_eq!(topology.evidence_snapshot().unwrap().domain_roots.len(), 4);
    assert!(args.is_empty());

    let root = topology
        .cfg
        .validator_dir(4)
        .join("ocomp")
        .join("domain-v1");
    assert!(root.join("protocol-bundle-v1.ocb1").is_file());
    assert!(!root.join("ocomp-key-v1.hex").exists());
    assert!(!root.join("ocomp-evm-key.hex").exists());
}

#[test]
fn node_owned_ocomp_roles_are_not_external_harness_processes() {
    for role in ["supervisor", "follower"] {
        assert!(serde_json::from_str::<OcompProcessRole>(&format!("\"{role}\"")).is_err());
    }
}

#[test]
fn clock_restart_suspends_and_preserves_the_exact_live_role_inventory() {
    if std::env::var_os(CHILD_MODE).is_some() {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }

    let mut topology = topology();
    topology.add_active_validator_domain(4).unwrap();
    for validator_index in 0..5_u8 {
        topology
            .attach_owned(
                Some(validator_index),
                OcompProcessRole::SnapshotExporter,
                None,
                child_guard(),
            )
            .unwrap();
    }
    for validator_index in [0_u8, 1, 4] {
        topology
            .attach_owned(
                Some(validator_index),
                OcompProcessRole::Worker,
                Some(0),
                child_guard(),
            )
            .unwrap();
    }

    let faults_before = topology.faults.clone();
    let resume = topology.suspend_node_facing_roles().unwrap();

    assert_eq!(resume.snapshot_exporters, vec![0, 1, 2, 3, 4]);
    assert_eq!(resume.workers, vec![(0, 0), (1, 0), (4, 0)]);
    assert_eq!(topology.faults, faults_before);
    assert!(topology
        .domains
        .iter()
        .all(|domain| { domain.snapshot_exporter.is_none() && domain.workers.is_empty() }));
}
