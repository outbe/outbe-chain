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
                Ok(
                    RetainedGcAttemptOutcome::PageProgress
                    | RetainedGcAttemptOutcome::NoLongerPending,
                ) => {}
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
        let projection_fence = self.projection_fence.clone();
        let _projection_guard = projection_fence
            .as_ref()
            .map(|fence| {
                fence
                    .gc_claim_guard()
                    .map_err(RetentionError::InvalidTransition)
            })
            .transpose()
            .map_err(RetainedGcAttemptFailure::global)?;
        let mut inner = self.lock().map_err(RetainedGcAttemptFailure::global)?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(RetainedGcAttemptFailure::global(error));
        }
        let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
        else {
            return Ok(RetainedGcAttemptOutcome::NoLongerPending);
        };
        if record.generation != work.generation {
            return Ok(RetainedGcAttemptOutcome::NoLongerPending);
        }
        let gc_record = match record.state {
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                export,
                terminal_height,
                release_height,
            } if release_height <= finalized_height => {
                if lease_has_other_references(&inner, key, candidate.input_lease_id)
                    || self.retained_tributes.is_none()
                {
                    return self
                        .persist_next(
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
                        .map_err(RetainedGcAttemptFailure::global);
                }
                let ack = self
                    .persist_next(
                        &mut inner,
                        key,
                        record,
                        PinStateV1::GcPending {
                            candidate,
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            source_generation,
                            export,
                            terminal_height,
                            release_height,
                        },
                    )
                    .map_err(RetainedGcAttemptFailure::global)?;
                inner
                    .registry
                    .as_ref()
                    .and_then(|registry| registry.records.get(&key))
                    .copied()
                    .filter(|current| current.generation == ack.generation)
                    .ok_or(RetentionError::InvalidTransition(
                        "GC claim disappeared after durable publication",
                    ))
                    .map_err(RetainedGcAttemptFailure::global)?
            }
            PinStateV1::GcPending { .. } => record,
            _ => return Ok(RetainedGcAttemptOutcome::NoLongerPending),
        };
        drop(inner);
        drop(_projection_guard);

        let (candidate, completed_state) = match gc_record.state {
            PinStateV1::GcPending {
                candidate,
                job_id,
                source_generation,
                export,
                ..
            } => (
                candidate,
                PinStateV1::Released {
                    candidate,
                    job_id,
                    source_generation,
                    observed_height: finalized_height,
                    export,
                },
            ),
            _ => unreachable!("retained GC work is durably claimed before MongoDB I/O"),
        };
        let complete = self
            .retained_tributes
            .as_ref()
            .expect("GcPending is unreachable without retained Tribute storage")
            .release_input_lease_page(candidate.input_lease_id)
            .map_err(|error| classify_retained_gc_failure(key, gc_record.generation, error))?;
        if !complete {
            return Ok(RetainedGcAttemptOutcome::PageProgress);
        }

        let mut inner = self.lock().map_err(RetainedGcAttemptFailure::global)?;
        let current = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
            .ok_or(RetentionError::InvalidTransition(
                "GC claim disappeared before completion",
            ))
            .map_err(RetainedGcAttemptFailure::global)?;
        if current != gc_record {
            // A canonical ACK can be durably attached while GC is doing Mongo
            // I/O outside this lock. Recheck deletion under its new generation
            // instead of publishing Released with the old ACK-less metadata.
            if gc_ack_metadata_advanced(gc_record, current) {
                return Ok(RetainedGcAttemptOutcome::NoLongerPending);
            }
            return Err(RetainedGcAttemptFailure::global(
                RetentionError::InvalidTransition("GC claim changed before completion"),
            ));
        }
        self.persist_next(&mut inner, key, current, completed_state)
            .map(RetainedGcAttemptOutcome::Completed)
            .map_err(RetainedGcAttemptFailure::global)
    }
}
