use super::*;

struct VoteInput<'vote> {
    vote: &'vote ResultVoteV1,
    inclusion_height: u64,
    scope: &'vote ExecutionScope,
    limits: &'vote SchemaLimits,
}

struct VerifiedVote {
    response: ResponseDeadlineKey,
    record: OcompJobRecordV1,
    member: ResolvedHistoricalResultVoteMemberV1,
}

impl MetadosisContract<'_> {
    /// Verifies the pinned vote and commits its accountability/quorum effects.
    pub fn record_ocomp_result_vote(
        &mut self,
        vote: &ResultVoteV1,
        inclusion_height: u64,
        scope: &ExecutionScope,
        limits: &SchemaLimits,
    ) -> Result<RecordedResultVoteV1> {
        let input = VoteInput {
            vote,
            inclusion_height,
            scope,
            limits,
        };
        let verified = self.verify_open_vote(&input)?;
        self.commit_verified_vote(&input, &verified)
    }

    fn verify_open_vote(&self, input: &VoteInput<'_>) -> Result<VerifiedVote> {
        let response = match self.response_window_for_job(input.vote.job_id)? {
            Some(response) => response,
            None => {
                let deadline_closed = self
                    .result_vote_accountability(input.vote.job_id, input.limits)?
                    .is_some_and(|accountability| accountability.closed_summary.is_some());
                if deadline_closed {
                    return Err(vote_reject(DEADLINE_PASSED));
                }
                return Err(reject("OCOMP result vote has no open response window"));
            }
        };
        let record = self
            .ocomp_job_record(response.intent_id, input.limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP response index points to a missing job")
            })?;
        let finalized = record.finalized.as_ref().ok_or_else(|| {
            storage_corruption_message("OCOMP response-window job is not finalized")
        })?;
        if finalized.job_id != response.job_id
            || finalized.deadline_height != response.deadline_height
        {
            return Err(storage_corruption_message(
                "OCOMP response index/job binding mismatch",
            ));
        }
        if !matches!(
            record.status,
            OcompJobStatus::VotingOpen | OcompJobStatus::Completed
        ) {
            return Err(reject(
                "OCOMP result vote requires an open or quorum-certified job",
            ));
        }
        if PinnedVoteBinding::from_prefix(&input.vote.prefix())
            != PinnedVoteBinding::from_record(&record)
        {
            return Err(reject(
                "OCOMP result vote does not match pinned job binding",
            ));
        }
        let member = self.verify_pinned_signature(input, &record)?;
        Ok(VerifiedVote {
            response,
            record,
            member,
        })
    }

    fn verify_pinned_signature(
        &self,
        input: &VoteInput<'_>,
        record: &OcompJobRecordV1,
    ) -> Result<ResolvedHistoricalResultVoteMemberV1> {
        let finalized = record.finalized.as_ref().ok_or_else(|| {
            storage_corruption_message("OCOMP response-window job is not finalized")
        })?;
        let snapshot = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
            self.storage.clone(),
            record.intent.result_validator_set_epoch,
            record.intent.result_committee_set_hash,
            record.intent.result_ocomp_binding_hash,
        )?
        .filter(|snapshot| snapshot.member_count == record.intent.result_member_count)
        .ok_or_else(|| reject("OCOMP result vote historical snapshot is missing"))?;
        let snapshot_key = outbe_validatorset::committee_snapshot_key(
            record.intent.result_validator_set_epoch,
            record.intent.result_committee_set_hash,
        );
        let member = resolve_historical_result_vote_member(
            self.storage.clone(),
            snapshot_key,
            snapshot.member_count,
            input.vote.ocomp_key_hash,
            input.vote.key_epoch,
        )?
        .ok_or_else(|| reject("OCOMP result vote member is missing"))?;
        input
            .vote
            .verify_historical_member(
                &record.intent,
                finalized.job_id,
                snapshot.member_count,
                member.key_epoch,
                &member.ocomp_public_key_sec1,
                input.inclusion_height,
                finalized.open_height,
                finalized.deadline_height,
                input.limits,
            )
            .map_err(|error| reject(format!("invalid OCOMP result vote: {error}")))?;

        Ok(member)
    }

    fn commit_verified_vote(
        &mut self,
        input: &VoteInput<'_>,
        verified: &VerifiedVote,
    ) -> Result<RecordedResultVoteV1> {
        let record = &verified.record;
        let finalized = record.finalized.as_ref().ok_or_else(|| {
            storage_corruption_message("OCOMP response-window job is not finalized")
        })?;
        let mut accountability = self
            .result_vote_accountability(finalized.job_id, input.limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP response-window vote slots are missing")
            })?;
        if accountability.quorum != finalized.quorum {
            return Err(storage_corruption_message(
                "OCOMP job/accountability quorum mismatch",
            ));
        }
        let had_quorum = accountability.quorum.is_some();
        let outcome = accountability
            .record_verified_vote(
                verified.member.validator_index,
                input.vote,
                input.inclusion_height,
                input.limits,
            )
            .map_err(|error| reject(format!("invalid OCOMP vote transition: {error}")))?;
        let quorum = accountability.quorum.clone();

        if !had_quorum {
            let authority = self
                .read_ocomp_activation_authority_for_bundle(
                    record.intent.protocol_bundle_hash,
                    input.limits,
                )?
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP activation authority is not installed")
                })?;
            if let Some(formed) = &quorum {
                self.apply_new_quorum(input, verified, formed, &authority)?;
            }
        } else if finalized.quorum != quorum {
            return Err(storage_corruption_message("OCOMP immutable quorum changed"));
        }
        self.write_result_vote_accountability(&accountability, input.limits)?;
        Ok(RecordedResultVoteV1 { outcome, quorum })
    }

    fn apply_new_quorum(
        &mut self,
        input: &VoteInput<'_>,
        verified: &VerifiedVote,
        formed: &OcompQuorumV1,
        authority: &super::super::activation::OcompActivationAuthorityV1,
    ) -> Result<()> {
        let record = &verified.record;
        if record.status != OcompJobStatus::VotingOpen {
            return Err(storage_corruption_message(
                "OCOMP quorum formed outside the voting-open transition",
            ));
        }
        let current_time =
            self.storage.timestamp()?.try_into().map_err(|_| {
                storage_corruption_message("OCOMP block timestamp does not fit u64")
            })?;
        let worldwide_day = outbe_primitives::time::WorldwideDay::new(record.intent.wwd);
        let aggregate = ValidatedWwdAggregate::load_and_validate(self.storage.clone())?;
        let outer = aggregate.record(worldwide_day).ok_or_else(|| {
            storage_corruption_message("OCOMP q-forming vote has no persisted outer WorldwideDay")
        })?;
        let completed_transition = reduce_outer_wwd(Some(outer), OuterWwdEvent::OcompCompleted)?;
        let storage = self.storage.clone();
        let apply_context = super::super::activation::QuorumApplyContext::new(
            &storage,
            input.scope,
            &completed_transition,
            input.inclusion_height,
            current_time,
            input.limits,
        );
        super::super::activation::apply_quorum_result(
            apply_context,
            self,
            super::super::activation::QuorumResultInput::new(
                verified.response.intent_id,
                &verified.record,
                &input.vote.result,
                formed,
                authority,
            ),
        )?;
        let applied = self
            .ocomp_job_record(verified.response.intent_id, input.limits)?
            .ok_or_else(|| storage_corruption_message("OCOMP q-forming apply removed the job"))?;
        if !matches!(applied.status, OcompJobStatus::Completed)
            || applied
                .finalized
                .as_ref()
                .and_then(|finalized| finalized.quorum.as_ref())
                != Some(formed)
        {
            return Err(storage_corruption_message(
                "OCOMP q-forming apply did not commit terminal quorum state",
            ));
        }
        Ok(())
    }
}
