use super::*;

pub(super) struct ComputeJob {
    pub(super) runner: Arc<SupervisorJobRunnerV1>,
    pub(super) adoption_config: SupervisorExportAdoptionConfig,
    pub(super) record: DiscoveryRecord,
    pub(super) generation: EmbeddedJobGenerationV1,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) sender: mpsc::Sender<EmbeddedComputeOutcomeV1>,
    #[cfg(feature = "test-protocol-overrides")]
    pub(super) local_result_mismatch_marker: PathBuf,
    #[cfg(feature = "test-protocol-overrides")]
    pub(super) mismatch_limits: SchemaLimits,
}

impl ComputeJob {
    pub(super) fn run(self) {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            let outcome = match self.attempt() {
                Ok(None) => {
                    thread::sleep(RETRY_INTERVAL);
                    continue;
                }
                Ok(Some(completed)) => EmbeddedComputeOutcomeV1::Completed {
                    generation: self.generation,
                    completed,
                },
                Err(error) => EmbeddedComputeOutcomeV1::Unrecoverable {
                    job_id: self.record.spec.summary.job_id,
                    generation: self.generation,
                    detail: error.to_string(),
                },
            };
            let _ = self.sender.send(outcome);
            return;
        }
    }

    fn attempt(
        &self,
    ) -> Result<Option<CompletedSupervisorJobV1>, crate::errors::EmbeddedComputeAttemptError> {
        let adoption = SupervisorExportAdoption::open(self.adoption_config.clone())?;
        let binding = match adoption.try_adopt(&self.record)? {
            SupervisorExportAdoptionOutcome::Pending => return Ok(None),
            SupervisorExportAdoptionOutcome::Adopted(binding) => binding,
        };
        let completed = match self
            .runner
            .run_to_result(&self.record, &binding, &self.cancelled)
        {
            Ok(completed) => completed,
            Err(error) if error.class() == SupervisorJobFailureClassV1::Retryable => {
                return Ok(None)
            }
            Err(error) => return Err(error.into()),
        };
        #[cfg(feature = "test-protocol-overrides")]
        let completed = CompletedSupervisorJobV1 {
            canonical_result: inject_local_result_mismatch_once(
                &self.local_result_mismatch_marker,
                completed.job_id,
                &completed.canonical_result,
                &self.mismatch_limits,
            )
            .map_err(crate::errors::EmbeddedComputeAttemptError::mismatch_override)?,
            ..completed
        };
        Ok(Some(completed))
    }
}
