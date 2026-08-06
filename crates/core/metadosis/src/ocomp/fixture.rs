use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    state::{LysisTerminalV1, OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    vote::OcompVoteAccountabilityV1,
    SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::MetadosisContract;

use super::{
    index::{insert_response_deadline_key, remove_response_deadline_key, ResponseDeadlineKey},
    state::{
        JobFsmLimits, JobFsmSnapshot, JobFsmState, LiveAttemptSnapshot, RetryTerminalOutcome,
        TerminalAttempt,
    },
};

impl MetadosisContract<'_> {
    /// Builds a canonical near-cap retry history for production-seam tests.
    ///
    /// The frozen profile remains authoritative. This helper writes only
    /// ordinary canonical job records and indexes, then restores the resulting
    /// FSM through the same validator used by production reads.
    #[cfg(test)]
    pub(crate) fn fixture_seed_ocomp_terminal_history(
        &mut self,
        live_intent_id: B256,
        terminal_records: u16,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<B256> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if terminal_records == 0 || terminal_records >= fsm_limits.max_terminal_records {
                return Err(fatal("invalid OCOMP terminal-history fixture request"));
            }

            let state = self
                .live_ocomp_fsm_state_by_intent(live_intent_id, schema_limits, fsm_limits)?
                .ok_or_else(|| fatal("terminal-history fixture has no live FSM"))?;
            let projection = state.projection();
            if projection.pending_nonce != 0
                || projection.terminal_records != 0
                || projection.deadline_height.is_none()
            {
                return Err(fatal(
                    "terminal-history fixture requires the first live voting attempt",
                ));
            }
            let base_record = self
                .ocomp_job_record(live_intent_id, schema_limits)?
                .ok_or_else(|| fatal("terminal-history fixture live job is missing"))?;
            if base_record.status != OcompJobStatus::VotingOpen || base_record.terminal.is_some() {
                return Err(fatal("terminal-history fixture requires a voting-open job"));
            }
            let wwd = outbe_common::WorldwideDay::new(base_record.intent.wwd);
            if self.terminal_intent_count(wwd)? != 0 {
                return Err(fatal("invalid OCOMP terminal-history fixture request"));
            }
            let base_finalized = base_record
                .finalized
                .as_ref()
                .ok_or_else(|| fatal("terminal-history fixture job is not finalized"))?
                .clone();
            let old_response_key = self
                .read_response_deadline_index()?
                .into_iter()
                .find(|key| key.intent_id == live_intent_id)
                .ok_or_else(|| fatal("terminal-history fixture response key is missing"))?;
            let retained_effect = state
                .snapshot()
                .live
                .ok_or_else(|| fatal("terminal-history fixture live snapshot is missing"))?
                .retained_effect;

            let mut attempts = Vec::new();
            attempts
                .try_reserve_exact(usize::from(terminal_records))
                .map_err(|_| fatal("allocate terminal-history fixture"))?;
            for nonce in 0..terminal_records {
                let pending_nonce = u64::from(nonce);
                let mut intent = base_record.intent.clone();
                intent.pending_nonce = pending_nonce;
                intent.attempt = u32::from(nonce);
                intent.activation_preconditions.metadosis.pending_nonce = pending_nonce;
                let intent_id = intent
                    .intent_id(schema_limits)
                    .map_err(|error| fatal(format!("derive fixture IntentId: {error}")))?;
                let mut finalized = base_finalized.clone();
                finalized.job_id = intent
                    .job_id(
                        finalized.finalized_request_block_hash,
                        finalized.finalized_request_state_root,
                        schema_limits,
                    )
                    .map_err(|error| fatal(format!("derive fixture JobId: {error}")))?;
                let terminal_height = finalized.deadline_height;
                let terminal_time = base_record
                    .intent
                    .logical_evaluation_time
                    .checked_add(pending_nonce)
                    .ok_or_else(|| fatal("fixture terminal time overflow"))?;
                let next_pending_nonce = pending_nonce
                    .checked_add(1)
                    .ok_or_else(|| fatal("fixture pending nonce overflow"))?;
                self.write_ocomp_job_record(
                    intent_id,
                    &OcompJobRecordV1 {
                        intent,
                        intent_height: base_record.intent_height,
                        status: OcompJobStatus::Expired,
                        finalized: Some(finalized),
                        terminal: Some(LysisTerminalV1 {
                            outcome: OcompTerminalOutcome::Expired,
                            terminal_height,
                            terminal_time,
                            next_pending_nonce: Some(next_pending_nonce),
                            completed_binding: None,
                        }),
                    },
                    schema_limits,
                )?;
                self.push_terminal_intent(wwd, intent_id, fsm_limits.max_terminal_records)?;
                attempts.push(TerminalAttempt {
                    intent_id,
                    pending_nonce,
                    terminal_height,
                    terminal_time,
                    next_pending_nonce,
                    outcome: RetryTerminalOutcome::Expired,
                });
            }

            let pending_nonce = u64::from(terminal_records);
            let mut current_intent = base_record.intent.clone();
            current_intent.pending_nonce = pending_nonce;
            current_intent.attempt = u32::from(terminal_records);
            current_intent
                .activation_preconditions
                .metadosis
                .pending_nonce = pending_nonce;
            let current_intent_id = current_intent
                .intent_id(schema_limits)
                .map_err(|error| fatal(format!("derive live fixture IntentId: {error}")))?;
            let mut current_finalized = base_finalized;
            current_finalized.job_id = current_intent
                .job_id(
                    current_finalized.finalized_request_block_hash,
                    current_finalized.finalized_request_state_root,
                    schema_limits,
                )
                .map_err(|error| fatal(format!("derive live fixture JobId: {error}")))?;
            let current_accountability = OcompVoteAccountabilityV1::empty(
                current_finalized.job_id,
                current_intent.result_validator_set_epoch,
                current_intent.result_committee_set_hash,
                current_intent.result_ocomp_binding_hash,
                current_intent.result_member_count,
                current_intent.result_quorum_threshold,
            )
            .map_err(|error| fatal(format!("build live fixture vote slots: {error}")))?;
            self.write_result_vote_accountability(&current_accountability, schema_limits)?;
            self.write_ocomp_job_record(
                current_intent_id,
                &OcompJobRecordV1 {
                    intent: current_intent,
                    intent_height: base_record.intent_height,
                    status: OcompJobStatus::VotingOpen,
                    finalized: Some(current_finalized.clone()),
                    terminal: None,
                },
                schema_limits,
            )?;

            let seeded = JobFsmState::restore(
                JobFsmSnapshot {
                    worldwide_day: projection.worldwide_day,
                    ready: None,
                    live: Some(LiveAttemptSnapshot {
                        intent_id: current_intent_id,
                        pending_nonce,
                        requested_height: base_record.intent_height,
                        deadline_height: Some(current_finalized.deadline_height),
                        retained_effect,
                    }),
                    terminal: attempts,
                },
                fsm_limits,
            )
            .map_err(|error| fatal(format!("restore terminal-history fixture: {error}")))?;

            self.remove_live_scheduler(live_intent_id)?;
            self.write_ocomp_state(&seeded)?;
            self.write_live_scheduler(&seeded)?;
            let mut response_index = self.read_response_deadline_index()?;
            remove_response_deadline_key(&mut response_index, old_response_key)?;
            insert_response_deadline_key(
                &mut response_index,
                ResponseDeadlineKey {
                    deadline_height: current_finalized.deadline_height,
                    job_id: current_finalized.job_id,
                    intent_id: current_intent_id,
                },
            )?;
            self.write_response_deadline_index(&response_index)?;

            let restored =
                self.ocomp_fsm_state(projection.worldwide_day, schema_limits, fsm_limits)?;
            let restored_projection = restored.projection();
            if restored_projection.terminal_records != terminal_records
                || restored_projection.pending_nonce != pending_nonce
                || restored_projection.live_intent_id != Some(current_intent_id)
            {
                return Err(fatal(
                    "terminal-history fixture failed canonical restore equivalence",
                ));
            }
            self.live_ocomp_fsm_states(schema_limits, fsm_limits)?;
            Ok(current_intent_id)
        })
    }
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
