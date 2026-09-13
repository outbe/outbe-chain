use crate::world::ocomp::*;

impl OcompTopology {
    /// Prove child-process liveness and mutual Worker/embedded-Supervisor
    /// registration for the complete baseline runtime.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_baseline_runtime_ready(
        &mut self,
        expected_workers_per_supervisor: usize,
    ) -> Result<OcompRuntimeCountsV1> {
        let deadline = Instant::now() + OCOMP_RUNTIME_READY_TIMEOUT;
        loop {
            self.ensure_baseline_processes_alive(expected_workers_per_supervisor)?;
            match self.observe_baseline_runtime(expected_workers_per_supervisor) {
                Ok(counts) => return Ok(counts),
                Err(error) if Instant::now() >= deadline => {
                    eyre::bail!(
                        "OCOMP Supervisors/Workers did not become ready within {} seconds: {error}",
                        OCOMP_RUNTIME_READY_TIMEOUT.as_secs()
                    );
                }
                Err(_) => sleep(Duration::from_millis(250)),
            }
        }
    }

    /// Fail immediately when a required owned OCOMP role exits. Registration
    /// convergence is retryable during startup; a dead child is not.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_baseline_processes_alive(
        &mut self,
        expected_workers_per_supervisor: usize,
    ) -> Result<()> {
        self.ensure_validator_roles_alive()?;
        for validator_index in self.validator_indices()? {
            for worker_ordinal in 0..u32::try_from(expected_workers_per_supervisor)? {
                self.ensure_worker_alive(validator_index, worker_ordinal)?;
            }
        }
        Ok(())
    }

    /// Probe the public Supervisor status surfaces without relying on retained
    /// child guards. This is used by the separate `localnet status` process.
    #[cfg(feature = "ocomp-integration")]
    pub fn observe_baseline_runtime(
        &self,
        expected_workers_per_supervisor: usize,
    ) -> Result<OcompRuntimeCountsV1> {
        let mut registered_workers = 0usize;
        let mut connected_workers = 0usize;
        for validator_index in self.validator_indices()? {
            let index = usize::from(validator_index);
            let address = SocketAddr::from(([127, 0, 0, 1], self.cfg.ocomp_endpoint_port(index)));
            let status = fetch_supervisor_status(address)?;
            ensure_supervisor_status_ready(
                validator_index,
                &status,
                expected_workers_per_supervisor,
            )?;
            registered_workers = registered_workers
                .checked_add(status.registered_workers)
                .ok_or_else(|| eyre::eyre!("OCOMP registered-worker count overflow"))?;
            connected_workers = connected_workers
                .checked_add(status.connected_workers)
                .ok_or_else(|| eyre::eyre!("OCOMP connected-worker count overflow"))?;
        }
        let supervisors = self.domains.len();
        let workers = supervisors
            .checked_mul(expected_workers_per_supervisor)
            .ok_or_else(|| eyre::eyre!("OCOMP worker count overflow"))?;
        Ok(OcompRuntimeCountsV1 {
            supervisors,
            snapshot_exporters: supervisors,
            workers,
            registered_workers,
            connected_workers,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_successor_workers_ready(&mut self) -> Result<()> {
        eyre::ensure!(
            self.successor_identity.is_some(),
            "successor Workers are not activated"
        );
        let deadline = Instant::now() + OCOMP_RUNTIME_READY_TIMEOUT;
        loop {
            let mut ready = true;
            let mut indices = self.validator_indices()?;
            if let Some((index, _)) = &self.keyless_full_node_domain {
                indices.push(*index);
                self.ensure_keyless_full_node_roles_alive(*index)?;
            }
            for validator_index in indices {
                self.ensure_worker_alive(validator_index, 1)?;
                let base = self.cfg.ocomp_endpoint_port(usize::from(validator_index));
                let successor_port = base
                    .checked_add(6)
                    .ok_or_else(|| eyre::eyre!("OCOMP successor lane endpoint port overflow"))?;
                match fetch_supervisor_status(SocketAddr::from(([127, 0, 0, 1], successor_port)))
                    .and_then(|status| ensure_supervisor_status_ready(validator_index, &status, 1))
                {
                    Ok(()) => {}
                    Err(_) => ready = false,
                }
            }
            if ready {
                // Do not accept registration observed just before an owned exit.
                for index in self.validator_indices()? {
                    self.ensure_worker_alive(index, 1)?;
                }
                if let Some((index, _)) = &self.keyless_full_node_domain {
                    self.ensure_keyless_full_node_roles_alive(*index)?;
                }
                return Ok(());
            }
            eyre::ensure!(
                Instant::now() < deadline,
                "OCOMP successor Workers did not register before the readiness deadline"
            );
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn ensure_keyless_full_node_roles_alive(
        &mut self,
        validator_index: u8,
    ) -> Result<()> {
        let (record_index, status) = {
            let process = self
                .keyless_full_node_domain_mut(validator_index)?
                .snapshot_exporter
                .as_mut()
                .ok_or_else(|| eyre::eyre!("FullNode SnapshotExporter is not owned"))?;
            (process.record_index, process.guard.exit_status()?)
        };
        if let Some(status) = status {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!("FullNode {validator_index} SnapshotExporter exited: {status}");
        }
        self.ensure_worker_alive(validator_index, 0)?;
        if self.successor_identity.is_some() {
            self.ensure_worker_alive(validator_index, 1)?;
        }
        Ok(())
    }

    /// Require both installed keyless compute lanes, without asserting any
    /// voting authority or treating a listening port as worker registration.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn ensure_keyless_full_node_roles_ready(&mut self, index: u8) -> Result<()> {
        let deadline = Instant::now() + OCOMP_RUNTIME_READY_TIMEOUT;
        loop {
            self.ensure_keyless_full_node_roles_alive(index)?;
            let lanes = if self.successor_identity.is_some() {
                2_u16
            } else {
                1_u16
            };
            let observed = (0..lanes)
                .try_for_each(|lane| {
                    let port = self
                        .cfg
                        .ocomp_endpoint_port(usize::from(index))
                        .checked_add(lane * 6)
                        .ok_or_else(|| eyre::eyre!("FullNode OCOMP lane port overflow"))?;
                    let status = fetch_supervisor_status(SocketAddr::from(([127, 0, 0, 1], port)))?;
                    ensure_supervisor_status_ready(index, &status, 1)
                })
                .and_then(|()| {
                    let port = self
                        .cfg
                        .ocomp_endpoint_port(usize::from(index))
                        .checked_add(12)
                        .ok_or_else(|| eyre::eyre!("FullNode exporter status port overflow"))?;
                    let status: outbe_ocomp::worker_observability::SnapshotExporterStatusV1 =
                        fetch_snapshot_exporter_status(SocketAddr::from(([127, 0, 0, 1], port)))?;
                    // The exporter builds every configured lane before entering
                    // reconciliation. A TCP listener alone can precede that work.
                    eyre::ensure!(
                        status.phase
                            == outbe_ocomp::worker_observability::SnapshotExporterPhaseV1::Idle
                            && status.last_error.is_none(),
                        "FullNode SnapshotExporter has not reconciled its installed lanes"
                    );
                    Ok(())
                });
            match observed {
                Ok(()) => {
                    self.ensure_keyless_full_node_roles_alive(index)?;
                    return Ok(());
                }
                Err(error) if Instant::now() >= deadline => {
                    return Err(error);
                }
                Err(_) => sleep(Duration::from_millis(250)),
            }
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_validator_roles_alive(&mut self) -> Result<()> {
        for validator_index in self.validator_indices()? {
            for role in [OcompProcessRole::SnapshotExporter] {
                let intentionally_stopped = self.faults.iter().any(|record| {
                    matches!(
                        (role, record.fault),
                        (
                            OcompProcessRole::SnapshotExporter,
                            OcompProcessFault::StopSnapshotExporter {
                                validator_index: stopped
                            }
                        ) if stopped == validator_index
                    )
                });
                let (record_index, exited) = {
                    let domain = self.domain_mut(validator_index)?;
                    let process = match role {
                        OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.as_mut(),
                        _ => unreachable!("fixed exporter iteration above"),
                    };
                    let Some(process) = process else {
                        if intentionally_stopped {
                            continue;
                        }
                        eyre::bail!(
                            "validator-{validator_index} OCOMP {role:?} is missing without a typed fault"
                        );
                    };
                    (process.record_index, process.guard.exited())
                };
                if exited {
                    self.records[record_index].stopped_at_millis = Some(unix_time_millis());
                    let role_name = match role {
                        OcompProcessRole::SnapshotExporter => "snapshot-exporter",
                        _ => unreachable!("fixed exporter iteration above"),
                    };
                    let log_path = self
                        .domain(validator_index)?
                        .root
                        .join(format!("{role_name}.log"));
                    eyre::bail!(
                        "validator-{validator_index} OCOMP {role_name} exited during startup:\n{}",
                        tail_file(&log_path, 20)
                    );
                }
            }
        }
        Ok(())
    }

    /// Require one exact harness-owned worker to still be running. A retained
    /// process record is not sufficient evidence after committee restarts: the
    /// child guard itself must report that the authenticated worker is live.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_worker_alive(&mut self, validator_index: u8, worker_ordinal: u32) -> Result<()> {
        let (record_index, status, log_path) = {
            let domain = self.compute_domain_mut(validator_index)?;
            let log_path = domain.root.join(format!("worker-{worker_ordinal}.log"));
            let process = domain.workers.get_mut(&worker_ordinal).ok_or_else(|| {
                eyre::eyre!("validator-{validator_index} worker-{worker_ordinal} is not registered")
            })?;
            (process.record_index, process.guard.exit_status()?, log_path)
        };
        if let Some(status) = status {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!(
                "node-{validator_index} worker-{worker_ordinal} exited ({status}):\n{}",
                tail_file(&log_path, 20)
            );
        }
        Ok(())
    }

    /// Validate the exact stopped incarnations, not merely an empty worker map.
    /// Call through expiry/retention and immediately before the intended restart.
    #[cfg(any(test, feature = "ocomp-integration"))]
    pub(crate) fn ensure_worker_cohort_stopped(
        &self,
        evidence: &crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
    ) -> Result<()> {
        let inventory = self
            .domains
            .iter()
            .map(|domain| domain.workers.len())
            .collect::<Vec<_>>();
        crate::internal::ocomp_worker_outage::require_stopped_cohort(&evidence.stops, &inventory)?;
        for stopped in &evidence.stops {
            let latest = self
                .records
                .iter()
                .rev()
                .find(|record| {
                    record.validator_index == Some(stopped.validator_index)
                        && record.role == OcompProcessRole::Worker
                })
                .ok_or_else(|| eyre::eyre!("stopped worker lacks owned process history"))?;
            eyre::ensure!(
                latest.pid == stopped.pid
                    && latest.worker_ordinal == Some(stopped.worker_ordinal)
                    && latest.stopped_at_millis == stopped.reaped_at_millis,
                "validator-{} worker incarnation changed after cohort fault",
                stopped.validator_index
            );
            eyre::ensure!(
                self.faults.iter().any(|record| {
                    record.fault
                        == (OcompProcessFault::StopWorker {
                            validator_index: stopped.validator_index,
                            worker_ordinal: stopped.worker_ordinal,
                        })
                        && record.applied_at_millis == stopped.signal_at_millis
                }),
                "stopped worker lacks its owned cohort fault record"
            );
        }
        Ok(())
    }

    /// Observe real exports only after the owned cohort has been fully reaped.
    /// Workers stay absent; nodes and exporters continue the ordinary public path.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn wait_for_exports_while_workers_stopped(
        &mut self,
        record: &outbe_ocomp_protocol::state::OcompJobRecordV1,
        checkpoint: crate::world::rpc::FinalizedCheckpoint,
        bundle: &ProtocolBundleV1,
        evidence: &mut crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
        timeout: Duration,
        mut ensure_network_alive: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        use crate::internal::ocomp_worker_outage::{observe_export, require_pre_open_cut};
        self.ensure_worker_cohort_stopped(evidence)?;
        eyre::ensure!(
            evidence.exports.is_empty(),
            "export observation cannot be replayed"
        );
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("worker outage job lacks canonical finality binding"))?;
        require_pre_open_cut(&evidence.cut_heads, finalized.open_height)?;
        let deadline = Instant::now() + timeout;
        loop {
            eyre::ensure!(
                Instant::now() < deadline,
                "four exact exports did not become available while workers were stopped"
            );
            ensure_network_alive()?;
            self.ensure_worker_cohort_stopped(evidence)?;
            self.ensure_validator_roles_alive()?;
            for validator_index in self.validator_indices()? {
                if !evidence
                    .exports
                    .iter()
                    .any(|item| item.validator_index == validator_index)
                {
                    if let Some(export) = observe_export(
                        self.domain_root(validator_index)?,
                        validator_index,
                        record,
                        checkpoint,
                        bundle,
                    )? {
                        evidence.exports.push(export);
                    }
                }
            }
            // Recheck ownership and typed exporter liveness after filesystem reads,
            // including on the successful iteration. An intentional exporter fault
            // from another scenario must not make missing exporters acceptable here.
            self.ensure_worker_cohort_stopped(evidence)?;
            for domain in &mut self.domains {
                let exporter = domain.snapshot_exporter.as_mut().ok_or_else(|| {
                    eyre::eyre!("worker outage requires every live snapshot exporter")
                })?;
                eyre::ensure!(
                    exporter.guard.exit_status()?.is_none(),
                    "snapshot exporter exited during worker outage"
                );
            }
            ensure_network_alive()?;
            eyre::ensure!(
                Instant::now() < deadline,
                "worker outage export observation exceeded its budget"
            );
            if evidence.exports.len() == 4 {
                return Ok(());
            }
            sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(feature = "ocomp-integration")]
const OCOMP_RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Live process and registration counts for the baseline validator OCOMP runtime.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OcompRuntimeCountsV1 {
    pub supervisors: usize,
    pub snapshot_exporters: usize,
    pub workers: usize,
    pub registered_workers: usize,
    pub connected_workers: usize,
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::world::ocomp) struct SupervisorWorkerStatusV1 {
    pub(in crate::world::ocomp) registry_generation: u64,
    pub(in crate::world::ocomp) registered_workers: usize,
    pub(in crate::world::ocomp) connected_workers: usize,
    pub(in crate::world::ocomp) busy_workers: usize,
    pub(in crate::world::ocomp) accepted_leases: usize,
    pub(in crate::world::ocomp) queued_units: usize,
    pub(in crate::world::ocomp) max_workers: usize,
}

#[cfg(feature = "ocomp-integration")]
fn fetch_supervisor_status(address: SocketAddr) -> Result<SupervisorWorkerStatusV1> {
    fetch_runtime_status(address, "/v1/status")
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn fetch_snapshot_exporter_status(
    address: SocketAddr,
) -> Result<outbe_ocomp::worker_observability::SnapshotExporterStatusV1> {
    fetch_runtime_status(address, "/status")
}

#[cfg(feature = "ocomp-integration")]
fn fetch_runtime_status<T: serde::de::DeserializeOwned>(
    address: SocketAddr,
    path: &str,
) -> Result<T> {
    const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| eyre::eyre!("connect to OCOMP runtime {address}{path}: {error}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .map_err(|error| eyre::eyre!("read OCOMP runtime {address}{path}: {error}"))?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .ok_or_else(|| eyre::eyre!("OCOMP runtime {address}{path} returned malformed HTTP"))?;
    let status_line_end = response
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| eyre::eyre!("OCOMP runtime {address}{path} returned no HTTP status"))?;
    let status_line = std::str::from_utf8(&response[..status_line_end])?.trim();
    eyre::ensure!(
        status_line.split_whitespace().nth(1) == Some("200"),
        "OCOMP runtime {address}{path} request failed: {status_line}"
    );
    serde_json::from_slice(&response[header_end..])
        .map_err(|error| eyre::eyre!("decode OCOMP runtime {address}{path}: {error}"))
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn ensure_supervisor_status_ready(
    validator_index: u8,
    status: &SupervisorWorkerStatusV1,
    expected_workers: usize,
) -> Result<()> {
    eyre::ensure!(
        status.registry_generation > 0,
        "validator-{validator_index} OCOMP Supervisor has no registry generation"
    );
    eyre::ensure!(
        status.max_workers >= expected_workers,
        "validator-{validator_index} OCOMP Supervisor capacity {} is below expected worker count {expected_workers}",
        status.max_workers
    );
    eyre::ensure!(
        status.registered_workers == expected_workers,
        "validator-{validator_index} OCOMP Supervisor reports {} registered workers, expected {expected_workers}",
        status.registered_workers
    );
    eyre::ensure!(
        status.connected_workers == expected_workers,
        "validator-{validator_index} OCOMP Supervisor reports {} connected workers, expected {expected_workers}",
        status.connected_workers
    );
    eyre::ensure!(
        status.busy_workers <= status.connected_workers,
        "validator-{validator_index} OCOMP Supervisor reports more busy than connected workers"
    );
    let _ = (status.accepted_leases, status.queued_units);
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn tail_file(path: &Path, max_lines: usize) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return format!("<unable to open {}>", path.display());
    };
    let size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = size.saturating_sub(16 * 1024);
    let _ = file.seek(SeekFrom::Start(start));
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    text.lines()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}
