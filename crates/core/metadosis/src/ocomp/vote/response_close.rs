use super::*;
use outbe_ocomp_protocol::vote::OcompVoteAccountabilityV1;

struct CloseInput<'limits> {
    key: ResponseDeadlineKey,
    at_height: u64,
    limits: &'limits SchemaLimits,
}

struct MissingParticipant {
    snapshot_key: B256,
    job_id: B256,
    index: u16,
}

impl MetadosisContract<'_> {
    pub(crate) fn close_due_ocomp_response_window(
        &mut self,
        at_height: u64,
        limits: &SchemaLimits,
    ) -> Result<ResponseWindowCloseReport> {
        let mut index = self.read_response_deadline_index()?;
        let Some(key) = index.first().copied() else {
            return Ok(ResponseWindowCloseReport::not_due());
        };
        if at_height < key.deadline_height {
            return Ok(ResponseWindowCloseReport::not_due());
        }
        let input = CloseInput {
            key,
            at_height,
            limits,
        };
        let record = self
            .ocomp_job_record(key.intent_id, limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP response index points to a missing job")
            })?;
        let accountability = self.close_accountability(&input, &record)?;
        let metrics = self.penalize_missing_votes(&input, &record, &accountability)?;
        self.write_result_vote_accountability(&accountability, limits)?;
        remove_response_deadline_key(&mut index, key)?;
        self.write_response_deadline_index(&index)?;
        let close = response_close_outcome(&record, key)?;
        Ok(ResponseWindowCloseReport { close, metrics })
    }

    fn close_accountability(
        &self,
        input: &CloseInput<'_>,
        record: &OcompJobRecordV1,
    ) -> Result<OcompVoteAccountabilityV1> {
        let finalized = record.finalized.as_ref().ok_or_else(|| {
            storage_corruption_message("OCOMP response-window job is not finalized")
        })?;
        if finalized.job_id != input.key.job_id
            || finalized.deadline_height != input.key.deadline_height
        {
            return Err(storage_corruption_message(
                "OCOMP response deadline/job binding mismatch",
            ));
        }
        let mut accountability = self
            .result_vote_accountability(input.key.job_id, input.limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP response-window vote slots are missing")
            })?;
        if accountability.quorum != finalized.quorum {
            return Err(storage_corruption_message(
                "OCOMP job/accountability quorum mismatch at close",
            ));
        }
        accountability
            .close(input.at_height, input.limits)
            .map_err(|error| {
                storage_corruption_message(format!("close OCOMP vote accountability: {error}"))
            })?;
        Ok(accountability)
    }

    fn penalize_missing_votes(
        &mut self,
        input: &CloseInput<'_>,
        record: &OcompJobRecordV1,
        accountability: &OcompVoteAccountabilityV1,
    ) -> Result<OcompPenaltyMetrics> {
        let snapshot = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
            self.storage.clone(),
            record.intent.result_validator_set_epoch,
            record.intent.result_committee_set_hash,
            record.intent.result_ocomp_binding_hash,
        )?
        .filter(|snapshot| snapshot.member_count == accountability.member_count)
        .ok_or_else(|| {
            storage_corruption_message("OCOMP deadline historical snapshot is missing")
        })?;
        let snapshot_key =
            outbe_validatorset::committee_snapshot_key(snapshot.epoch, snapshot.committee_set_hash);

        let mut metrics = OcompPenaltyMetrics::default();
        for (index, slot) in accountability.slots.iter().enumerate() {
            if slot.is_some() {
                continue;
            }
            let index = u16::try_from(index).map_err(|_| {
                storage_corruption_message("OCOMP missing participant index exceeds u16")
            })?;
            self.penalize_missing_participant(
                &MissingParticipant {
                    snapshot_key,
                    job_id: input.key.job_id,
                    index,
                },
                &mut metrics,
            )?;
        }
        Ok(metrics)
    }

    fn penalize_missing_participant(
        &mut self,
        missing: &MissingParticipant,
        metrics: &mut OcompPenaltyMetrics,
    ) -> Result<()> {
        let participant_index = missing.index;
        let snapshot_key = missing.snapshot_key;
        let member = outbe_validatorset::read_ocomp_snapshot_member_at(
            self.storage.clone(),
            snapshot_key,
            participant_index,
        )?
        .ok_or_else(|| storage_corruption_message("OCOMP deadline snapshot member is missing"))?;

        let mut staking = outbe_staking::contract::Staking::new(self.storage.clone());
        resolve_recovery_before_miss(
            &mut staking,
            member.validator_address,
            participant_index,
            metrics,
        )?;
        self.record_active_miss(missing, member.validator_address, &mut staking, metrics)
    }

    fn record_active_miss(
        &mut self,
        missing: &MissingParticipant,
        validator_address: Address,
        staking: &mut outbe_staking::contract::Staking<'_>,
        metrics: &mut OcompPenaltyMetrics,
    ) -> Result<()> {
        let participant_index = missing.index;
        let validators = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        let current = validators.get_validator(validator_address)?;
        if current
            .is_some_and(|record| record.status == outbe_validatorset::runtime::status::ACTIVE)
        {
            let penalty =
                staking
                    .record_ocomp_miss(validator_address)
                    .map_err(|error| match error {
                        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                            storage_corruption_message(format!(
                            "record ACTIVE missing OCOMP validator {participant_index}: {error}"
                        ))
                        }
                        other => other,
                    })?;
            self.emit(IMetadosis::OcompVoteMissed {
                validator: validator_address,
                jobId: missing.job_id,
                missCount: penalty.miss_count,
                slashedBonded: penalty.slashed_bonded,
                recoveryDeadline: penalty.recovery_deadline,
                firstInWindow: penalty.first_in_window,
            })?;
            metrics.misses.push((
                validator_address,
                penalty.first_in_window,
                penalty.recovery_deadline,
            ));
        }

        Ok(())
    }
}

fn resolve_recovery_before_miss(
    staking: &mut outbe_staking::contract::Staking<'_>,
    validator_address: Address,
    participant_index: u16,
    metrics: &mut OcompPenaltyMetrics,
) -> Result<()> {
    let resolution = staking
        .resolve_due_ocomp_recovery_window(validator_address)
        .map_err(|error| match error {
            PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                storage_corruption_message(format!(
                    "resolve due OCOMP recovery for participant {participant_index}: {error}"
                ))
            }
            other => other,
        })?;
    let outcome = match resolution {
        outbe_staking::logic::OcompRecoveryResolution::Restored { .. } => Some("restored"),
        outbe_staking::logic::OcompRecoveryResolution::Jailed { observability, .. } => {
            metrics.punishments.push(observability);
            Some("jailed")
        }
        outbe_staking::logic::OcompRecoveryResolution::ClosedNonActive { .. } => Some("non_active"),
        outbe_staking::logic::OcompRecoveryResolution::NotOpen
        | outbe_staking::logic::OcompRecoveryResolution::NotDue { .. } => None,
    };
    if let Some(outcome) = outcome {
        metrics
            .recovery_resolutions
            .push((validator_address, outcome));
    }

    Ok(())
}

fn response_close_outcome(
    record: &OcompJobRecordV1,
    key: ResponseDeadlineKey,
) -> Result<ResponseWindowCloseV1> {
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| storage_corruption_message("OCOMP response-window job is not finalized"))?;
    let close = match record.status {
        OcompJobStatus::VotingOpen if finalized.quorum.is_none() => {
            ResponseWindowCloseV1::NoQuorum {
                intent_id: key.intent_id,
            }
        }
        OcompJobStatus::Completed if finalized.quorum.is_some() => {
            ResponseWindowCloseV1::QuorumPreserved {
                intent_id: key.intent_id,
            }
        }
        _ => {
            return Err(storage_corruption_message(
                "OCOMP response close found an invalid job status",
            ))
        }
    };

    Ok(close)
}
