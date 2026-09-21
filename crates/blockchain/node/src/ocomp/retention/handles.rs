use crate::ocomp::retention::*;

/// Process-local retention selector that can be shared with projection before
/// the provider-backed coordinator is available.
///
/// Installation is one-way: every clone of the surrounding `Arc` observes the
/// exact same coordinator, and an attempted replacement fails closed.
pub struct SharedOcompRetentionSelector {
    coordinator: OnceLock<Arc<OcompRetentionCoordinator>>,
    gc_signal: Arc<RetainedGcSignal>,
}

impl Default for SharedOcompRetentionSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedOcompRetentionSelector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            coordinator: OnceLock::new(),
            gc_signal: Arc::new(RetainedGcSignal::default()),
        }
    }

    pub fn install(
        &self,
        coordinator: Arc<OcompRetentionCoordinator>,
    ) -> Result<(), RetentionError> {
        self.coordinator
            .set(Arc::clone(&coordinator))
            .map_err(|_| RetentionError::RetentionCoordinatorAlreadyInstalled)?;
        spawn_retained_gc_worker(Arc::downgrade(&coordinator), Arc::clone(&self.gc_signal))?;
        self.gc_signal.wake();
        Ok(())
    }

    /// Returns the exact finalized retention generation used by snapshot handoff.
    pub fn finalized_job_record(
        &self,
        job_id: B256,
    ) -> Result<(u64, FinalizedJobPinV1), RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .finalized_job_record(job_id)
    }

    /// Returns restart-safe source-generation candidates for discovery handoff.
    ///
    /// A terminal journal may have been reached directly from `Finalized` or
    /// through `Exported`; both exact predecessor generations are returned so
    /// the durable discovery spool can select its existing authority.
    pub fn discovery_job_records(
        &self,
        job_id: B256,
    ) -> Result<Vec<(u64, FinalizedJobPinV1)>, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .discovery_job_records(job_id)
    }

    /// Binds a finalized request pin to the exact finalized job stored in
    /// canonical Metadosis state. No finality or response-window height is
    /// inferred locally.
    pub fn bind_canonical_finalized_job(
        &self,
        candidate_block_hash: B256,
        record: &OcompJobRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .bind_canonical_finalized_job(candidate_block_hash, record)
    }

    /// Durably binds an exporter ACK to the exact finalized retention generation.
    pub fn confirm_export_ack(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .confirm_export_ack(job_id, source_generation, lease_generation, manifest_hash)
    }

    /// Recovers an exact spool ACK using the canonical finalized job authority.
    pub fn confirm_canonical_export_ack(
        &self,
        record: &OcompJobRecordV1,
        export: ExportAuthorityV1,
    ) -> Result<DurablePinAck, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .confirm_canonical_export_ack(record, export)
    }

    /// Closes a newly bound job at the already sampled finalized target.
    pub fn reconcile_canonical_terminal(
        &self,
        record: &OcompJobRecordV1,
        observed_height: u64,
    ) -> Result<(), RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .reconcile_canonical_terminal(record, observed_height)
    }

    /// Returns the exact durable authority for an already released export.
    pub fn released_export_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ExportAuthorityV1>, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .released_export_authority(job_id)
    }

    /// Returns the complete durable retirement authority, including the
    /// source generation for ACK-less canonical expiry.
    pub fn released_job_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ReleasedJobAuthorityV1>, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .released_job_authority(job_id)
    }

    /// Reconcile one frame already loaded by the single finalized reader.
    pub fn reconcile_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> Result<(), RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .reconcile_finalized_frame(frame, observation)?;
        self.gc_signal.publish_finalized(frame.identity().number);
        Ok(())
    }

    /// Publishes finalized progress without performing work on the ExEx task.
    pub fn notify_finalized_height(&self, height: u64) {
        self.gc_signal.publish_finalized(height);
    }

    /// Publishes the durable contiguous closure checkpoint used to retire
    /// journal tombstones. This is an atomic update plus a worker wakeup.
    pub fn notify_closure_checkpoint(&self, height: u64) {
        self.gc_signal.publish_closure(height);
    }
}

impl TributeRetentionSelector for OcompRetentionCoordinator {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        if self.retained_tributes.is_none() {
            return Err(RetentionError::RetainedTributeStorageUnavailable.to_string());
        }
        let inner = self.lock().map_err(|error| error.to_string())?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error.to_string());
        }
        let mut selected = BTreeSet::new();
        for record in inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.values())
        {
            match record.state {
                PinStateV1::AwaitingJobFinalization { candidate }
                    if candidate.wwd == worldwide_day.value() =>
                {
                    selected.insert(candidate.input_lease_id);
                }
                PinStateV1::Finalized { candidate, .. }
                | PinStateV1::Exported { candidate, .. }
                | PinStateV1::Terminal { candidate, .. }
                    if candidate.wwd == worldwide_day.value() =>
                {
                    selected.insert(candidate.input_lease_id);
                }
                PinStateV1::GcPending { .. } => {}
                _ => {}
            }
        }
        match selected.len() {
            0 => Ok(None),
            1 => Ok(Some(RetainedTributePin {
                input_lease_id: *selected.first().expect("one selected retention key"),
                worldwide_day,
            })),
            _ => Err("multiple input retention identities exist for one WWD".to_owned()),
        }
    }
}

impl TributeRetentionSelector for SharedOcompRetentionSelector {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        self.coordinator
            .get()
            .ok_or_else(|| RetentionError::RetentionCoordinatorNotInstalled.to_string())?
            .active_pin_for(worldwide_day)
    }
}
