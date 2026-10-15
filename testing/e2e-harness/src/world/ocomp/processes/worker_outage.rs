//! Worker-cohort outage: exact stopped incarnations and exports observed meanwhile.

use crate::world::ocomp::*;

/// The finalized job whose exports a worker outage must still observe.
#[cfg(feature = "ocomp-integration")]
pub(crate) struct OutageExportJob<'a> {
    pub(crate) record: &'a outbe_ocomp_protocol::state::OcompJobRecordV1,
    pub(crate) checkpoint: crate::world::rpc::FinalizedCheckpoint,
    pub(crate) bundle: &'a ProtocolBundleV1,
}

impl OcompTopology {
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
        job: &OutageExportJob<'_>,
        evidence: &mut crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
        timeout: Duration,
        mut ensure_network_alive: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        use crate::internal::ocomp_worker_outage::require_pre_open_cut;
        self.ensure_worker_cohort_stopped(evidence)?;
        eyre::ensure!(
            evidence.exports.is_empty(),
            "export observation cannot be replayed"
        );
        let finalized = job
            .record
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
            self.collect_missing_exports(job, evidence)?;
            // Recheck ownership and typed exporter liveness after filesystem reads,
            // including on the successful iteration. An intentional exporter fault
            // from another scenario must not make missing exporters acceptable here.
            self.ensure_worker_cohort_stopped(evidence)?;
            self.ensure_every_snapshot_exporter_live()?;
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

    #[cfg(feature = "ocomp-integration")]
    fn collect_missing_exports(
        &self,
        job: &OutageExportJob<'_>,
        evidence: &mut crate::internal::ocomp_worker_outage::WorkerOutageEvidence,
    ) -> Result<()> {
        use crate::internal::ocomp_worker_outage::observe_export;
        for validator_index in self.validator_indices()? {
            if evidence
                .exports
                .iter()
                .any(|item| item.validator_index == validator_index)
            {
                continue;
            }
            if let Some(export) = observe_export(
                self.domain_root(validator_index)?,
                validator_index,
                job.record,
                job.checkpoint,
                job.bundle,
            )? {
                evidence.exports.push(export);
            }
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn ensure_every_snapshot_exporter_live(&mut self) -> Result<()> {
        for domain in &mut self.domains {
            let exporter = domain.snapshot_exporter.as_mut().ok_or_else(|| {
                eyre::eyre!("worker outage requires every live snapshot exporter")
            })?;
            eyre::ensure!(
                exporter.guard.exit_status()?.is_none(),
                "snapshot exporter exited during worker outage"
            );
        }
        Ok(())
    }
}
