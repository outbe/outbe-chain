use super::coordinator::*;
use crate::ocomp::retention::*;

impl OcompRetentionCoordinator {
    pub fn finalized_live_jobs(&self) -> Result<Vec<FinalizedJobPinV1>, RetentionError> {
        let inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        let mut jobs = Vec::new();
        for record in inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.values())
        {
            match record.state {
                PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }
                | PinStateV1::Exported {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    ..
                } => jobs.push(FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }),
                PinStateV1::AwaitingJobFinalization { .. }
                | PinStateV1::Terminal { .. }
                | PinStateV1::GcPending { .. }
                | PinStateV1::Released { .. } => {}
            }
        }
        jobs.sort_by_key(|job| (job.candidate.block_number, job.candidate.block_hash));
        Ok(jobs)
    }

    pub fn finalized_job_record(
        &self,
        job_id: B256,
    ) -> Result<(u64, FinalizedJobPinV1), RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => Ok((
                record.generation,
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
            )),
            _ => Err(RetentionError::InvalidTransition(
                "snapshot handoff requires the exact finalized Job",
            )),
        }
    }

    pub fn discovery_job_records(
        &self,
        job_id: B256,
    ) -> Result<Vec<(u64, FinalizedJobPinV1)>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let (pin, generations) = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(record.generation), None],
            ),
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(export.source_generation), None],
            ),
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                ..
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(source_generation), None],
            ),
            PinStateV1::GcPending {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                ..
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(source_generation), None],
            ),
            PinStateV1::AwaitingJobFinalization { .. } | PinStateV1::Released { .. } => {
                return Err(RetentionError::InvalidTransition(
                    "discovery requires a live or terminal finalized job",
                ));
            }
        };
        let generations = generations
            .into_iter()
            .flatten()
            .map(|generation| (generation, pin))
            .collect::<Vec<_>>();
        if generations.is_empty() {
            return Err(RetentionError::GenerationOverflow);
        }
        Ok(generations)
    }

    pub fn released_export_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ExportAuthorityV1>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(match record.state {
            PinStateV1::Released {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::AwaitingJobFinalization { .. }
            | PinStateV1::Finalized { .. }
            | PinStateV1::Exported { .. }
            | PinStateV1::Terminal { .. }
            | PinStateV1::GcPending { .. }
            | PinStateV1::Released { .. } => None,
        })
    }

    pub fn released_job_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ReleasedJobAuthorityV1>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(match record.state {
            PinStateV1::Released {
                candidate,
                job_id: existing,
                source_generation,
                export,
                ..
            } if existing == job_id => Some(ReleasedJobAuthorityV1 {
                candidate,
                job_id,
                source_generation,
                export,
            }),
            _ => None,
        })
    }

    /// Reconcile retention from the exact block and receipts owned by the
    /// unified finalized reader, without a separate receipt-provider query.
    pub fn reconcile_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> Result<(), RetentionError> {
        if let Some(error) = retention_status_error(&self.status()) {
            return Err(error);
        }
        let height = frame.identity().number;
        if let Some(observation) = observation {
            let candidate = self
                .source
                .candidate_for_finalized_observation(frame, observation)?;
            self.record_finalized_observation(candidate)?;
        }
        let live = {
            let inner = self.lock()?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Finalized {
                        candidate, job_id, ..
                    }
                    | PinStateV1::Exported {
                        candidate, job_id, ..
                    } => Some((record.generation, candidate, job_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (generation, candidate, job_id) in live {
            // A durable journal can be ahead of the replay cursor. Its later
            // jobs do not exist in this historical state yet.
            if candidate.block_number > height {
                continue;
            }
            if let Some(terminal_height) = self
                .source
                .terminal_height_at_finalized_frame(frame, candidate, job_id)?
            {
                self.observe_terminal(job_id, generation, terminal_height.max(height))?;
            }
        }
        Ok(())
    }

    /// Reobserving finalized history must preserve an already advanced lease.
    pub(in crate::ocomp::retention) fn record_finalized_observation(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        if let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&candidate.block_hash))
            .copied()
        {
            if record_candidate(record) != candidate {
                return Err(RetentionError::ConflictingCandidate);
            }
            return Ok(ack_for(record));
        }
        let ack = self.record_new_candidate_locked(&mut inner, candidate)?;
        tracing::info!(
            target: "outbe::ocomp::retention",
            block_number = candidate.block_number,
            block_hash = %candidate.block_hash,
            intent_id = %candidate.intent_id,
            wwd = candidate.wwd,
            input_lease_id = %candidate.input_lease_id,
            generation = ack.generation,
            "registered OCOMP request from finalized block"
        );
        Ok(ack)
    }

    pub(in crate::ocomp::retention) fn record_new_candidate_locked(
        &self,
        inner: &mut CoordinatorInner,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if inner.registry.as_ref().is_some_and(|registry| {
            registry.records.values().any(|record| {
                matches!(record.state, PinStateV1::GcPending { .. })
                    && record_candidate(*record).input_lease_id == candidate.input_lease_id
            })
        }) {
            return Err(RetentionError::InvalidTransition(
                "input lease garbage collection is already in progress",
            ));
        }
        if inner.registry.as_ref().is_some_and(|registry| {
            registry
                .records
                .values()
                .filter(|record| !matches!(record.state, PinStateV1::Released { .. }))
                .count()
                >= JOURNAL_RECORD_COUNT_MAX
        }) {
            return Err(RetentionError::RegistryCapacity);
        }
        let generation = next_registry_generation(inner)?;
        self.persist_locked(
            inner,
            candidate.block_hash,
            PinRecordV1 {
                generation,
                state: PinStateV1::AwaitingJobFinalization { candidate },
            },
        )
    }

    /// Finalizes one retained request exclusively from its canonical typed
    /// Metadosis record. The request event remains a locator; the chain record
    /// is the authority for JobId and every response-window height.
    pub fn bind_canonical_finalized_job(
        &self,
        candidate_block_hash: B256,
        record: &OcompJobRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let candidate = {
            let inner = self.lock()?;
            if let Some(error) = retention_status_error(&inner.status) {
                return Err(error);
            }
            inner
                .registry
                .as_ref()
                .and_then(|registry| registry.records.get(&candidate_block_hash))
                .copied()
                .map(record_candidate)
                .ok_or(RetentionError::InvalidTransition(
                    "canonical finalized job has no retained request candidate",
                ))?
        };
        if candidate.block_hash != candidate_block_hash {
            return Err(RetentionError::ConflictingCandidate);
        }
        self.finalize_exact(canonical_finalized_pin(candidate, record)?)
    }

    /// Replay can bind a historical candidate after the terminal frame was
    /// already observed. Apply that same canonical state without waiting for
    /// another block, and never move a retired record backwards.
    pub fn reconcile_canonical_terminal(
        &self,
        canonical: &OcompJobRecordV1,
        observed_height: u64,
    ) -> Result<(), RetentionError> {
        let finalized = canonical
            .finalized
            .as_ref()
            .ok_or(RetentionError::InvalidTransition(
                "canonical OCOMP job is not finalized",
            ))?;
        let record = {
            let inner = self.lock()?;
            let (_, record) = record_for_job(&inner, finalized.job_id)?;
            record
        };
        let candidate = record_candidate(record);
        canonical_finalized_pin(candidate, canonical)?;
        if matches!(
            record.state,
            PinStateV1::Finalized { .. } | PinStateV1::Exported { .. }
        ) {
            if let Some(height) = terminal_height_from_record(
                canonical,
                observed_height,
                candidate,
                finalized.job_id,
            )? {
                self.observe_terminal(finalized.job_id, record.generation, height)?;
            }
        }
        Ok(())
    }

    /// A crash may persist the spool ACK before retaining its export authority.
    /// Completing that metadata write never reactivates a retired lease. Expired
    /// jobs cannot adopt a late ACK; every ACK requires canonical job authority.
    pub fn confirm_canonical_export_ack(
        &self,
        canonical: &OcompJobRecordV1,
        export: ExportAuthorityV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if canonical.status == OcompJobStatus::Expired {
            return Err(RetentionError::InvalidTransition(
                "expired job cannot adopt an export ACK",
            ));
        }
        let finalized = canonical
            .finalized
            .as_ref()
            .ok_or(RetentionError::InvalidTransition(
                "canonical OCOMP job is not finalized",
            ))?;
        {
            let inner = self.lock()?;
            let (_, record) = record_for_job(&inner, finalized.job_id)?;
            canonical_finalized_pin(record_candidate(record), canonical)?;
        }
        match self.confirm_export_ack(
            finalized.job_id,
            export.source_generation,
            export.lease_generation,
            export.manifest_hash,
        ) {
            Ok(ack) => return Ok(ack),
            Err(RetentionError::InvalidTransition(_)) => {}
            Err(error) => return Err(error),
        }
        if !matches!(
            canonical.status,
            OcompJobStatus::Completed | OcompJobStatus::Failed
        ) || export.source_generation == 0
            || export.lease_generation == 0
            || export.manifest_hash.is_zero()
        {
            return Err(RetentionError::InvalidTransition(
                "late export ACK requires exact terminal authority",
            ));
        }
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, finalized.job_id)?;
        canonical_finalized_pin(record_candidate(record), canonical)?;
        let mut state = record.state;
        let slot = match &mut state {
            PinStateV1::Terminal {
                source_generation,
                export: slot,
                ..
            }
            | PinStateV1::GcPending {
                source_generation,
                export: slot,
                ..
            } if *source_generation == export.source_generation => slot,
            PinStateV1::Released {
                source_generation,
                export: slot,
                ..
            } if *source_generation == export.source_generation => slot,
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "late export ACK has a different source generation",
                ))
            }
        };
        match *slot {
            Some(existing) if existing == export => return Ok(ack_for(record)),
            Some(_) => {
                return Err(RetentionError::InvalidTransition(
                    "late export ACK conflicts with retained authority",
                ))
            }
            None => *slot = Some(export),
        }
        self.persist_next(&mut inner, key, record, state)
    }

    pub fn record_exported(
        &self,
        job_id: B256,
        expected_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        if exact_export_replay(
            record,
            job_id,
            expected_generation,
            lease_generation,
            manifest_hash,
        ) {
            return Ok(ack_for(record));
        }
        ensure_generation(record, expected_generation)?;
        let (candidate, finality_recorded_height, open_height, deadline_height) = match record.state
        {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
            ),
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "export requires the exact finalized job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export: ExportAuthorityV1 {
                    source_generation: expected_generation,
                    lease_generation,
                    manifest_hash,
                },
            },
        )
    }

    /// Confirm a durable spool ACK across restart windows. A live finalized
    /// record performs the Exported transition; later states accept only the
    /// exact source generation that must have preceded them.
    pub fn confirm_export_ack(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        if source_generation == 0 || lease_generation == 0 || manifest_hash.is_zero() {
            return Err(RetentionError::InvalidTransition(
                "export ACK authority is incomplete",
            ));
        }
        match self.record_exported(job_id, source_generation, lease_generation, manifest_hash) {
            Ok(ack) => return Ok(ack),
            Err(RetentionError::InvalidTransition(_) | RetentionError::StaleGeneration { .. }) => {}
            Err(error) => return Err(error),
        }
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let export = match record.state {
            PinStateV1::Terminal {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::GcPending {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::Released {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            _ => None,
        };
        if export
            == Some(ExportAuthorityV1 {
                source_generation,
                lease_generation,
                manifest_hash,
            })
        {
            Ok(ack_for(record))
        } else {
            Err(RetentionError::InvalidTransition(
                "export ACK does not match the retained source generation",
            ))
        }
    }

    pub fn replay_exported(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(exact_export_replay(
            record,
            job_id,
            source_generation,
            lease_generation,
            manifest_hash,
        )
        .then(|| ack_for(record)))
    }

    pub fn observe_terminal(
        &self,
        job_id: B256,
        expected_generation: u64,
        terminal_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let release_height = terminal_height
            .checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS)
            .ok_or(RetentionError::InvalidTransition(
                "terminal release height overflows",
            ))?;
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        ensure_generation(record, expected_generation)?;
        let (
            candidate,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
        ) = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
                record.generation,
                None,
            ),
            PinStateV1::Exported {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
                export.source_generation,
                Some(export),
            ),
            PinStateV1::Terminal {
                job_id: existing,
                terminal_height: existing_terminal,
                release_height: existing_release,
                ..
            } if existing == job_id
                && existing_terminal == terminal_height
                && existing_release == release_height =>
            {
                return Ok(ack_for(record));
            }
            PinStateV1::GcPending {
                job_id: existing,
                terminal_height: existing_terminal,
                release_height: existing_release,
                ..
            } if existing == job_id
                && existing_terminal == terminal_height
                && existing_release == release_height =>
            {
                return Ok(ack_for(record));
            }
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "terminal transition requires the exact live job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            key,
            record,
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
            },
        )
    }

    pub fn is_signable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    pub fn is_exportable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Finalized {
                    job_id: current, ..
                } | PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    pub(in crate::ocomp::retention) fn live_candidate(
        &self,
        job_id: B256,
    ) -> Result<CandidatePinV1, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: current,
                ..
            }
            | PinStateV1::Exported {
                candidate,
                job_id: current,
                ..
            } if current == job_id => Ok(candidate),
            _ => Err(RetentionError::InvalidTransition(
                "proof construction requires the exact live finalized job",
            )),
        }
    }

    pub(in crate::ocomp::retention) fn finalize_exact(
        &self,
        finalized: FinalizedJobPinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let key = finalized.candidate.block_hash;
        let record = record_for_candidate(&inner, finalized.candidate)?;
        match record.state {
            PinStateV1::AwaitingJobFinalization { candidate }
                if candidate == finalized.candidate =>
            {
                self.persist_next(
                    &mut inner,
                    key,
                    record,
                    PinStateV1::Finalized {
                        candidate,
                        job_id: finalized.job_id,
                        finality_recorded_height: finalized.finality_recorded_height,
                        open_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                    },
                )
            }
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if candidate == finalized.candidate
                && job_id == finalized.job_id
                && finality_recorded_height == finalized.finality_recorded_height
                && open_height == finalized.open_height
                && deadline_height == finalized.deadline_height =>
            {
                Ok(ack_for(record))
            }
            _ => Err(RetentionError::InvalidTransition(
                "canonical job does not match its finalized request",
            )),
        }
    }
}
