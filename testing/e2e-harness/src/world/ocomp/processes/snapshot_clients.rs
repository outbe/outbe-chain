//! Selected-node snapshot pause: stop and restore one node's external clients.

use crate::world::ocomp::*;

impl OcompTopology {
    /// Stop only one selected node's currently owned external clients. This
    /// checks every owner before the first stop. It records no protocol fault.
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
