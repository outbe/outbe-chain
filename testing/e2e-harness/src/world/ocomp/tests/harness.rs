use super::*;

pub(super) const CHILD_MODE: &str = "OUTBE_OCOMP_TOPOLOGY_CHILD";

pub(super) struct TestTopology {
    _directory: tempfile::TempDir,
    topology: OcompTopology,
}

impl std::ops::Deref for TestTopology {
    type Target = OcompTopology;

    fn deref(&self) -> &Self::Target {
        &self.topology
    }
}

impl std::ops::DerefMut for TestTopology {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.topology
    }
}

pub(super) fn topology_with_validators(validators: usize) -> TestTopology {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        data_dir: directory.path().to_path_buf(),
        validators,
        ..Environment::default()
    };
    env.ports.start_scenario(env.validators).unwrap();
    TestTopology {
        _directory: directory,
        topology: OcompTopology::new(Config::for_scenario(&env, 1)),
    }
}

pub(super) fn topology() -> TestTopology {
    topology_with_validators(Environment::default().validators)
}

pub(super) fn stopped_outage_fixture() -> (
    TestTopology,
    crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
) {
    use crate::internal::ocomp_worker_outage::{WorkerOutageEvidence, WorkerStopEvidence};
    let mut topology = topology_with_validators(4);
    let mut evidence = WorkerOutageEvidence::default();
    for index in 0..4 {
        let pid = 100 + u32::from(index);
        topology.records.push(OcompProcessRecordV1 {
            validator_index: Some(index),
            role: OcompProcessRole::Worker,
            worker_ordinal: Some(0),
            pid,
            started_at_millis: 1,
            stopped_at_millis: Some(20),
        });
        topology.faults.push(OcompFaultRecordV1 {
            fault: OcompProcessFault::StopWorker {
                validator_index: index,
                worker_ordinal: 0,
            },
            applied_at_millis: 10,
        });
        evidence.stops.push(WorkerStopEvidence {
            validator_index: index,
            worker_ordinal: 0,
            pid,
            signal_at_millis: 10,
            signal_error: None,
            reaped_at_millis: Some(20),
            exit_code: None,
            exit_signal: Some(9),
            wait_error: None,
        });
    }
    (topology, evidence)
}

pub(super) fn child_guard() -> ChildGuard {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("world::ocomp::tests::process_isolation::typed_fault_stops_only_the_selected_owned_process")
        .arg("--nocapture")
        .env(CHILD_MODE, "1");
    ChildGuard::spawn("ocomp topology child", command).unwrap()
}
