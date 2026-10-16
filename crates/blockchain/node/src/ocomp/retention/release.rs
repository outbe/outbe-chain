use super::coordinator::*;
use crate::ocomp::retention::*;

impl OcompRetentionCoordinator {
    pub fn release_due(
        &self,
        finalized_height: u64,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        for work in self.gc_candidate_work(finalized_height)? {
            match self.release_due_work(work, finalized_height) {
                Ok(RetainedGcAttemptOutcome::Completed(ack)) => return Ok(Some(ack)),
                Ok(RetainedGcAttemptOutcome::NoLongerPending) => {}
                Err(error) => return Err(error.into_error()),
            }
        }
        Ok(None)
    }

    pub(in crate::ocomp::retention) fn gc_candidate_work(
        &self,
        finalized_height: u64,
    ) -> Result<Vec<RetainedGcWorkId>, RetentionError> {
        let inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        Ok(inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.iter())
            .filter_map(|(key, record)| match record.state {
                PinStateV1::Terminal { release_height, .. }
                    if release_height <= finalized_height =>
                {
                    Some(RetainedGcWorkId {
                        key: *key,
                        generation: record.generation,
                    })
                }
                PinStateV1::GcPending { .. } => Some(RetainedGcWorkId {
                    key: *key,
                    generation: record.generation,
                }),
                _ => None,
            })
            .collect())
    }

    pub(in crate::ocomp::retention) fn release_due_work(
        &self,
        work: RetainedGcWorkId,
        finalized_height: u64,
    ) -> Result<RetainedGcAttemptOutcome, RetainedGcAttemptFailure> {
        let key = work.key;
        let mut inner = self.lock().map_err(RetainedGcAttemptFailure::global)?;
        let Some(record) = current_retained_gc_record(&inner, work)? else {
            return Ok(RetainedGcAttemptOutcome::NoLongerPending);
        };
        let (candidate, job_id, source_generation, export) = match record.state {
            PinStateV1::Terminal {
                candidate,
                job_id,
                source_generation,
                export,
                release_height,
                ..
            } if release_height <= finalized_height => {
                (candidate, job_id, source_generation, export)
            }
            PinStateV1::GcPending {
                candidate,
                job_id,
                source_generation,
                export,
                ..
            } => (candidate, job_id, source_generation, export),
            _ => return Ok(RetainedGcAttemptOutcome::NoLongerPending),
        };
        // Release only the job's local journal pin. Tribute ciphertext and day
        // indexes remain in canonical EVM state; there is no body deletion.
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Released {
                candidate,
                job_id,
                source_generation,
                observed_height: finalized_height,
                export,
            },
        )
        .map(RetainedGcAttemptOutcome::Completed)
        .map_err(RetainedGcAttemptFailure::global)
    }
}

fn current_retained_gc_record(
    inner: &CoordinatorInner,
    work: RetainedGcWorkId,
) -> Result<Option<PinRecordV1>, RetainedGcAttemptFailure> {
    if let Some(error) = retention_status_error(&inner.status) {
        return Err(RetainedGcAttemptFailure::global(error));
    }
    Ok(inner
        .registry
        .as_ref()
        .and_then(|registry| registry.records.get(&work.key))
        .copied()
        .filter(|record| record.generation == work.generation))
}
