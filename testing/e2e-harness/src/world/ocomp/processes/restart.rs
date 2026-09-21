use crate::world::ocomp::*;

impl OcompTopology {
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn snapshot_worker_port(&self, slot: usize) -> u16 {
        self.cfg.ocomp_worker_port(slot, 0)
    }

    /// Stop only one selected node's currently owned external clients. Every
    /// owner is checked before the first stop; no protocol fault is recorded.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn stop_node_facing_roles_for_snapshot(
        &mut self,
        index: u8,
    ) -> Result<StoppedNodeFacingRoles> {
        let keyless = self
            .keyless_full_node_domain
            .as_ref()
            .is_some_and(|(slot, _)| *slot == index);
        let successor = self
            .successor_identity
            .map(|identity| identity.protocol_bundle_hash);
        let (root, exporter, worker_ordinals) = {
            let domain = self.compute_domain_mut(index)?;
            let exporter = domain.snapshot_exporter.is_some();
            let workers = domain.workers.keys().copied().collect::<Vec<_>>();
            eyre::ensure!(
                exporter || !workers.is_empty(),
                "selected node has no owned OCOMP clients"
            );
            if keyless {
                let expected = if successor.is_some() {
                    vec![0, 1]
                } else {
                    vec![0]
                };
                eyre::ensure!(
                    exporter && workers == expected,
                    "FullNode client inventory differs from its existing ordinary launch recipe"
                );
            }
            for process in domain
                .workers
                .values_mut()
                .chain(domain.snapshot_exporter.iter_mut())
            {
                eyre::ensure!(
                    process.guard.exit_status()?.is_none(),
                    "selected OCOMP child PID {} exited before requested stop",
                    process.guard.pid()
                );
            }
            (domain.root.clone(), exporter, workers)
        };
        let mut stops = Vec::new();
        // Workers leave first; exporter stays attached until their output writers stop.
        for ordinal in &worker_ordinals {
            stops.push(self.stop_snapshot_client(index, Some(*ordinal))?);
        }
        if exporter {
            stops.push(self.stop_snapshot_client(index, None)?);
        }
        Ok(StoppedNodeFacingRoles {
            index,
            root,
            keyless,
            exporter,
            worker_ordinals,
            stops,
            launch_bundle: self
                .launch_identity
                .map(|identity| identity.protocol_bundle_hash),
            successor_bundle: successor,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    fn stop_snapshot_client(
        &mut self,
        index: u8,
        ordinal: Option<u32>,
    ) -> Result<OcompClientStopObservation> {
        use std::os::unix::process::ExitStatusExt as _;
        let (record_index, observed, accepted) = {
            let domain = self.compute_domain_mut(index)?;
            let process = match ordinal {
                Some(ordinal) => domain.workers.get_mut(&ordinal),
                None => domain.snapshot_exporter.as_mut(),
            }
            .ok_or_else(|| eyre::eyre!("selected OCOMP owner disappeared before stop"))?;
            eyre::ensure!(
                process.guard.exit_status()?.is_none(),
                "selected OCOMP owner exited before signal"
            );
            let pid = process.guard.pid();
            let requested = unix_time_millis();
            let status = process.guard.stop_and_reap()?;
            let observed = OcompClientStopObservation {
                index,
                role: if ordinal.is_some() {
                    OcompProcessRole::Worker
                } else {
                    OcompProcessRole::SnapshotExporter
                },
                worker_ordinal: ordinal,
                pid,
                stop_requested_at_millis: requested,
                reaped_at_millis: unix_time_millis(),
                code: status.code(),
                signal: status.signal(),
            };
            (
                process.record_index,
                observed,
                status.success() || status.signal() == Some(15),
            )
        };
        self.records[record_index].stopped_at_millis = Some(observed.reaped_at_millis);
        eyre::ensure!(
            accepted,
            "selected OCOMP PID {} failed during stop: code={:?} signal={:?}",
            observed.pid,
            observed.code,
            observed.signal
        );
        let domain = self.compute_domain_mut(index)?;
        match ordinal {
            Some(ordinal) => {
                domain.workers.remove(&ordinal);
            }
            None => {
                domain.snapshot_exporter.take();
            }
        }
        Ok(observed)
    }

    /// Restore only the recorded selected inventory after its node is ordinarily
    /// ready. Existing helpers keep their own endpoint/readiness checks.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn resume_snapshot_node_facing_roles(
        &mut self,
        stopped: StoppedNodeFacingRoles,
    ) -> Result<()> {
        let keyless = self
            .keyless_full_node_domain
            .as_ref()
            .is_some_and(|(slot, _)| *slot == stopped.index);
        eyre::ensure!(
            keyless == stopped.keyless,
            "selected compute role changed while stopped"
        );
        eyre::ensure!(
            self.launch_identity
                .map(|identity| identity.protocol_bundle_hash)
                == stopped.launch_bundle
                && self
                    .successor_identity
                    .map(|identity| identity.protocol_bundle_hash)
                    == stopped.successor_bundle,
            "selected compute bundle changed while stopped"
        );
        let domain = self.compute_domain_mut(stopped.index)?;
        eyre::ensure!(
            domain.root == stopped.root
                && domain.snapshot_exporter.is_none()
                && domain.workers.is_empty(),
            "selected compute domain changed or acquired another owner while stopped"
        );
        if stopped.keyless {
            return self.start_keyless_full_node_roles(stopped.index);
        }
        if stopped.exporter {
            self.restart_snapshot_exporter(stopped.index)?;
        }
        for ordinal in stopped.worker_ordinals {
            self.restart_worker(stopped.index, ordinal)?;
        }
        Ok(())
    }

    /// Stop every currently attached node-facing OCOMP process without
    /// recording a protocol fault, returning the exact inventory to restore.
    /// Deliberately absent workers therefore remain absent after the restart.
    #[cfg(any(test, feature = "ocomp-integration"))]
    pub(crate) fn suspend_node_facing_roles(
        &mut self,
    ) -> Result<crate::world::ocomp::OcompNodeFacingResumePlan> {
        let mut snapshot_exporters = Vec::new();
        let mut workers = Vec::new();

        for index in 0..self.domains.len() {
            let validator_index = u8::try_from(index)
                .map_err(|_| eyre::eyre!("validator index exceeds the harness wire format"))?;
            let (exporter, attached_workers) = {
                let domain = &mut self.domains[index];
                (
                    domain.snapshot_exporter.take(),
                    std::mem::take(&mut domain.workers),
                )
            };
            if let Some(exporter) = exporter {
                snapshot_exporters.push(validator_index);
                self.stop_owned(exporter);
            }
            for (worker_ordinal, worker) in attached_workers {
                workers.push((validator_index, worker_ordinal));
                self.stop_owned(worker);
            }
        }

        Ok(OcompNodeFacingResumePlan {
            snapshot_exporters,
            workers,
        })
    }

    /// Restore exactly one inventory returned by
    /// [`Self::suspend_node_facing_roles`] after node recovery.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn resume_node_facing_roles(
        &mut self,
        plan: OcompNodeFacingResumePlan,
    ) -> Result<()> {
        for validator_index in plan.snapshot_exporters {
            self.restart_snapshot_exporter(validator_index)?;
        }
        for (validator_index, worker_ordinal) in plan.workers {
            self.restart_worker(validator_index, worker_ordinal)?;
        }
        self.ensure_validator_roles_alive()
    }

    /// Stop only the compute clients; the synchronized FullNode process and
    /// durable domain remain intact for validator-mode promotion.
    #[cfg(feature = "ocomp-integration")]
    pub fn stop_keyless_full_node_roles(&mut self, validator_index: u8) -> Result<()> {
        let (exporter, workers) = {
            let domain = self.keyless_full_node_domain_mut(validator_index)?;
            (
                domain.snapshot_exporter.take(),
                std::mem::take(&mut domain.workers),
            )
        };
        let exporter =
            exporter.ok_or_else(|| eyre::eyre!("FullNode snapshot exporter is not running"))?;
        self.stop_owned(exporter);
        for (_, worker) in workers {
            self.stop_owned(worker);
        }
        Ok(())
    }

    /// Arm the test-only local-result mutation for one keyless FullNode job.
    /// The production binary claims and binds this empty marker to the first
    /// observed JobId; the harness never supplies a digest or result payload.
    #[cfg(feature = "ocomp-integration")]
    pub fn arm_keyless_full_node_result_mismatch(&self, validator_index: u8) -> Result<PathBuf> {
        let root = self
            .keyless_full_node_domain(validator_index)?
            .root
            .join("test-faults");
        fs::create_dir_all(&root)?;
        let marker = root.join("local-result-mismatch.once");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&marker)?;
        file.sync_all()?;
        File::open(&root)?.sync_all()?;
        Ok(marker)
    }

    /// Restart one external Worker after a typed stop. The embedded Supervisor
    /// remains owned by the node throughout the fault.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_worker(&mut self, validator_index: u8, worker_ordinal: u32) -> Result<()> {
        self.restart_worker_cohort(&[(validator_index, worker_ordinal)])
    }

    /// Start the whole selected cohort before waiting for any one worker.
    /// This preserves the single-worker restart contract without serial startup
    /// sleeps letting the first workers finish before the last one is launched.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn restart_worker_cohort(&mut self, workers: &[(u8, u32)]) -> Result<()> {
        eyre::ensure!(
            !workers.is_empty(),
            "worker restart cohort must not be empty"
        );
        for (position, &(validator_index, worker_ordinal)) in workers.iter().enumerate() {
            eyre::ensure!(
                !workers[..position].contains(&(validator_index, worker_ordinal)),
                "duplicate worker in restart cohort"
            );
            eyre::ensure!(
                !self
                    .domain(validator_index)?
                    .workers
                    .contains_key(&worker_ordinal),
                "validator-{validator_index} worker-{worker_ordinal} is already running"
            );
        }
        for &(validator_index, worker_ordinal) in workers {
            self.spawn_restarted_worker(validator_index, worker_ordinal)?;
        }
        sleep(Duration::from_secs(2));
        for &(validator_index, worker_ordinal) in workers {
            self.ensure_worker_alive(validator_index, worker_ordinal)?;
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn spawn_restarted_worker(&mut self, validator_index: u8, worker_ordinal: u32) -> Result<()> {
        if self
            .domain(validator_index)?
            .workers
            .contains_key(&worker_ordinal)
        {
            eyre::bail!("validator-{validator_index} worker-{worker_ordinal} is already running");
        }
        let initial_identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let (bundle_lane, identity) = if worker_ordinal == 1 {
            self.successor_identity
                .map_or((0, initial_identity), |identity| (1, identity))
        } else {
            (0, initial_identity)
        };
        let domain_root = self.domain(validator_index)?.root.clone();
        let guard = self.spawn_worker_process(
            validator_index,
            worker_ordinal,
            bundle_lane,
            domain_root,
            identity,
        )?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Worker,
            Some(worker_ordinal),
            guard,
        )?;
        Ok(())
    }

    /// Restart the fixed SnapshotExporter role in one domain after a typed stop.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_snapshot_exporter(&mut self, validator_index: u8) -> Result<()> {
        self.restart_snapshot_exporter_inner(validator_index, EXPORTER_RESTART_ATTEMPTS)
    }

    /// Fault the complete baseline cohort without waiting for a job or export.
    /// The caller captures cut heads immediately afterwards and binds them to
    /// the canonical job's exclusive pre-open boundary once it is available.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn stop_worker_cohort(
        &mut self,
        evidence: &mut crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
    ) -> Result<()> {
        use crate::internal::ocomp_worker_outage::{terminate_cohort, WorkerStopEvidence};
        eyre::ensure!(
            self.domains.len() == 4 && self.faults.len() + 4 <= OCOMP_MAX_FAULT_RECORDS,
            "worker fault requires the complete four-validator cohort"
        );
        eyre::ensure!(
            evidence.exports.is_empty()
                && evidence.stops.is_empty()
                && evidence.cut_heads.is_empty(),
            "worker fault cannot be replayed"
        );
        for (index, domain) in self.domains.iter_mut().enumerate() {
            eyre::ensure!(domain.workers.len() == 1, "unexpected worker inventory");
            let worker = domain
                .workers
                .get_mut(&0)
                .ok_or_else(|| eyre::eyre!("missing baseline worker"))?;
            eyre::ensure!(
                worker.guard.exit_status()?.is_none(),
                "worker exited before cohort fault"
            );
            evidence.stops.push(WorkerStopEvidence {
                validator_index: u8::try_from(index)?,
                worker_ordinal: 0,
                pid: worker.guard.pid(),
                signal_at_millis: 0,
                signal_error: None,
                reaped_at_millis: None,
                exit_code: None,
                exit_signal: None,
                wait_error: None,
            });
        }
        let mut workers = self
            .domains
            .iter_mut()
            .map(|domain| &mut domain.workers.get_mut(&0).unwrap().guard)
            .collect::<Vec<_>>();
        let outcome = terminate_cohort(&mut workers, &mut evidence.stops);
        drop(workers);
        for stopped in &evidence.stops {
            if let Some(reaped_at) = stopped.reaped_at_millis {
                let process = self
                    .domain_mut(stopped.validator_index)?
                    .workers
                    .remove(&0)
                    .unwrap();
                self.records[process.record_index].stopped_at_millis = Some(reaped_at);
                self.faults.push(OcompFaultRecordV1 {
                    fault: OcompProcessFault::StopWorker {
                        validator_index: stopped.validator_index,
                        worker_ordinal: 0,
                    },
                    applied_at_millis: stopped.signal_at_millis,
                });
            }
        }
        outcome
    }

    #[cfg(feature = "ocomp-integration")]
    fn restart_snapshot_exporter_inner(
        &mut self,
        validator_index: u8,
        attempts_left: u8,
    ) -> Result<()> {
        if self.domain(validator_index)?.snapshot_exporter.is_some() {
            eyre::bail!("validator-{validator_index} snapshot exporter is already running");
        }
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let exporter = self.spawn_validator_role(
            validator_index,
            OcompProcessRole::SnapshotExporter,
            identity,
        )?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::SnapshotExporter,
            None,
            exporter,
        )?;
        sleep(Duration::from_secs(2));
        let (record_index, exited) = {
            let process = self
                .domain_mut(validator_index)?
                .snapshot_exporter
                .as_mut()
                .expect("attached immediately above");
            (process.record_index, process.guard.exited())
        };
        if exited && attempts_left > 0 {
            // The killed predecessor keeps its projection writer lease until the
            // lease lapses, so an immediate successor loses the race with it.
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            if let Some(process) = self.domain_mut(validator_index)?.snapshot_exporter.take() {
                self.stop_owned(process);
            }
            sleep(WRITER_LEASE_LAPSE);
            return self.restart_snapshot_exporter_inner(validator_index, attempts_left - 1);
        }
        if exited {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!(
                "validator-{validator_index} OCOMP snapshot exporter exited during typed restart:\n{}",
                tail_file(
                    &self
                        .domain(validator_index)?
                        .root
                        .join("snapshot-exporter.log"),
                    20
                )
            );
        }
        Ok(())
    }

    /// Restart every external compute client while preserving the domain data.
    /// The embedded Supervisor restarts with the node and is not a harness-owned
    /// process.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_node_facing_processes(&mut self, validator_index: u8) -> Result<()> {
        let (exporter, workers) = {
            let domain = self.domain_mut(validator_index)?;
            (
                domain.snapshot_exporter.take(),
                std::mem::take(&mut domain.workers),
            )
        };
        if let Some(process) = exporter {
            self.stop_owned(process);
        }
        let worker_ordinals = workers.keys().copied().collect::<Vec<_>>();
        for (_, process) in workers {
            self.stop_owned(process);
        }
        self.restart_snapshot_exporter(validator_index)?;
        for worker_ordinal in worker_ordinals {
            self.restart_worker(validator_index, worker_ordinal)?;
        }
        Ok(())
    }

    /// Stop owned clients before their nodes and retain unexpected exit failures.
    pub(crate) fn stop_clients_for_teardown(&mut self) -> Result<()> {
        use std::os::unix::process::ExitStatusExt as _;
        let mut processes = Vec::new();
        for domain in self.domains.iter_mut().chain(
            self.keyless_full_node_domain
                .iter_mut()
                .map(|(_, domain)| domain),
        ) {
            processes.extend(std::mem::take(&mut domain.workers).into_values());
            processes.extend(domain.snapshot_exporter.take());
        }
        let mut failures = Vec::new();
        for mut process in processes {
            let result = (|| -> Result<()> {
                let before = process.guard.exit_status()?;
                let status = process.guard.stop_and_reap()?;
                // These external clients may use the OS default SIGTERM
                // handler. Only accept that signal when we sent the stop to
                // this live incarnation; an earlier crash is not cleanup.
                eyre::ensure!(
                    status.success() || (before.is_none() && status.signal() == Some(15)),
                    "OCOMP child PID {} exited with {status}",
                    process.guard.pid()
                );
                Ok(())
            })();
            self.records[process.record_index].stopped_at_millis = Some(unix_time_millis());
            if let Err(error) = result {
                failures.push(format!("{error:#}"));
            }
        }
        eyre::ensure!(
            failures.is_empty(),
            "OCOMP teardown failed: {}",
            failures.join("; ")
        );
        Ok(())
    }

    /// Execute one authorized process fault without accepting a PID/path/command.
    pub fn apply_process_fault(&mut self, fault: OcompProcessFault) -> Result<()> {
        if self.faults.len() >= OCOMP_MAX_FAULT_RECORDS {
            eyre::bail!("OCOMP scenario reached the bounded fault-record limit");
        }
        match fault {
            OcompProcessFault::StopSnapshotExporter { validator_index } => {
                let process = self
                    .domain_mut(validator_index)?
                    .snapshot_exporter
                    .take()
                    .ok_or_else(|| eyre::eyre!("snapshot exporter is not running"))?;
                self.stop_owned(process);
            }
            OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal,
            } => {
                let process = self
                    .domain_mut(validator_index)?
                    .workers
                    .remove(&worker_ordinal)
                    .ok_or_else(|| eyre::eyre!("worker is not running"))?;
                self.stop_owned(process);
            }
        }
        self.faults.push(OcompFaultRecordV1 {
            fault,
            applied_at_millis: unix_time_millis(),
        });
        Ok(())
    }

    pub(in crate::world::ocomp) fn stop_owned(&mut self, mut process: OwnedProcess) {
        process.guard.stop();
        self.records[process.record_index].stopped_at_millis = Some(unix_time_millis());
    }
}

/// Longer than the projection writer lease a killed exporter leaves behind.
#[cfg(feature = "ocomp-integration")]
const WRITER_LEASE_LAPSE: Duration = Duration::from_secs(7);

#[cfg(feature = "ocomp-integration")]
const EXPORTER_RESTART_ATTEMPTS: u8 = 3;

const OCOMP_MAX_FAULT_RECORDS: usize = 32;

/// Exact external OCOMP process inventory quiesced around a node clock restart.
/// Embedded Supervisors remain node-owned; this plan records only the roles the
/// harness must recreate after every node has crossed the common finality gate.
#[cfg(any(test, feature = "ocomp-integration"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OcompNodeFacingResumePlan {
    pub(in crate::world::ocomp) snapshot_exporters: Vec<u8>,
    pub(in crate::world::ocomp) workers: Vec<(u8, u32)>,
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug)]
pub(crate) struct OcompClientStopObservation {
    pub(crate) index: u8,
    pub(crate) role: OcompProcessRole,
    pub(crate) worker_ordinal: Option<u32>,
    pub(crate) pid: u32,
    pub(crate) stop_requested_at_millis: u64,
    pub(crate) reaped_at_millis: u64,
    pub(crate) code: Option<i32>,
    pub(crate) signal: Option<i32>,
}

/// An opaque, single-use inventory for one selected existing compute domain.
#[cfg(feature = "ocomp-integration")]
#[derive(Debug)]
pub(crate) struct StoppedNodeFacingRoles {
    index: u8,
    root: PathBuf,
    keyless: bool,
    exporter: bool,
    worker_ordinals: Vec<u32>,
    launch_bundle: Option<B256>,
    successor_bundle: Option<B256>,
    stops: Vec<OcompClientStopObservation>,
}
#[cfg(feature = "ocomp-integration")]
impl StoppedNodeFacingRoles {
    pub(crate) fn index(&self) -> u8 {
        self.index
    }
    pub(crate) fn stops(&self) -> &[OcompClientStopObservation] {
        &self.stops
    }
}

#[cfg(all(test, feature = "ocomp-integration"))]
mod snapshot_client_tests {
    use super::*;
    use crate::env::Environment;
    use std::process::Command;

    fn fixture() -> (tempfile::TempDir, OcompTopology) {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        (dir, OcompTopology::new(Config::resolve(&env)))
    }
    fn attach(topology: &mut OcompTopology, index: u8, ordinal: u32, exited: bool) -> u32 {
        let mut command = Command::new("sh");
        if exited {
            command.args(["-c", "exit 0"]);
        } else {
            command.args(["-c", "exec sleep 30"]);
        }
        let mut guard = ChildGuard::spawn("selected-client-fixture", command).unwrap();
        if exited {
            guard.reap_fault(std::time::Duration::from_secs(2)).unwrap();
        }
        let pid = guard.pid();
        topology
            .attach_owned(Some(index), OcompProcessRole::Worker, Some(ordinal), guard)
            .unwrap();
        pid
    }
    #[test]
    fn selected_stop_rejects_missing_and_preexited_clients_before_stopping_live_peer() {
        let (_dir, mut topology) = fixture();
        assert!(topology.stop_node_facing_roles_for_snapshot(99).is_err());
        assert!(topology.stop_node_facing_roles_for_snapshot(0).is_err());
        let live = attach(&mut topology, 0, 0, false);
        attach(&mut topology, 0, 1, true);
        assert!(topology.stop_node_facing_roles_for_snapshot(0).is_err());
        let guard = &mut topology
            .domain_mut(0)
            .unwrap()
            .workers
            .get_mut(&0)
            .unwrap()
            .guard;
        assert_eq!(guard.pid(), live);
        assert!(guard.exit_status().unwrap().is_none());
        assert!(topology
            .records
            .iter()
            .all(|record| record.stopped_at_millis.is_none()));
    }
    #[test]
    fn selected_stop_reaps_only_selected_domain_and_preserves_absent_roles() {
        let (_dir, mut topology) = fixture();
        let selected = attach(&mut topology, 0, 2, false);
        let peer = attach(&mut topology, 1, 0, false);
        let stopped = topology.stop_node_facing_roles_for_snapshot(0).unwrap();
        assert_eq!(stopped.index(), 0);
        assert_eq!(stopped.stops().len(), 1);
        assert_eq!(stopped.stops()[0].pid, selected);
        assert_eq!(stopped.stops()[0].signal, Some(15));
        assert_eq!(stopped.worker_ordinals, vec![2]);
        assert!(!stopped.exporter);
        assert!(topology.domain(0).unwrap().workers.is_empty());
        let guard = &mut topology
            .domain_mut(1)
            .unwrap()
            .workers
            .get_mut(&0)
            .unwrap()
            .guard;
        assert_eq!(guard.pid(), peer);
        assert!(guard.exit_status().unwrap().is_none());
        assert!(topology.faults.is_empty());
    }
}
