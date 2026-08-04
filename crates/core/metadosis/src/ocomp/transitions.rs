use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_lysis::activation_v1::LysisTerminalPermitV1;
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, JobIntentV1},
    receipts::{ActivationOutcome, AggregateActivationReceiptV1, RequestBudgetSplitReceiptV1},
    state::{
        ActiveGenerationV1, LysisTerminalV1, OcompCompletedBindingV1, OcompFinalizedJobV1,
        OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome, RESULT_VOTE_MIN_FINALITY_DEPTH,
    },
    vote::OcompVoteAccountabilityV1,
    SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_promislimit::{ocomp_budget::CarryOverCredit, schema::PromisLimitContract};

use crate::{
    aggregate::WwdStatus,
    commit::commit_outer_transition,
    precompile::IMetadosis,
    reducer::{OcompRetryCause, OuterWwdTransition, OuterWwdTransitionKind},
    schema::{MetadosisContract, WorldwideDayEntryExt},
};

use super::{
    codec::{decode_live_scheduler_index, MAX_LIVE_JOBS},
    index::{
        insert_ready_key, insert_response_deadline_key, remove_ready_key, ReadyIndexKey,
        ResponseDeadlineKey,
    },
    state::{JobFsmCommand, JobFsmLimits, RetryTerminalOutcome},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OcompExpiryDisposition {
    RetryScheduled {
        next_pending_nonce: u64,
    },
    TerminalNoRetry {
        next_pending_nonce: u64,
        carry_over: CarryOverCredit,
    },
}

impl MetadosisContract<'_> {
    /// Commits one canonical live job and all Metadosis-owned indexes.
    ///
    /// The caller has already applied or replay-validated the owner budget
    /// effect. This method nevertheless requires and stores the exact receipt,
    /// closing the persisted receipt/state equivalence.
    pub(crate) fn commit_ocomp_request(
        &mut self,
        outer_transition: &OuterWwdTransition,
        intent: &JobIntentV1,
        receipt: &RequestBudgetSplitReceiptV1,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<()> {
        (|| {
            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompRequestCommitted
            ) {
                return Err(fatal(
                    "OCOMP request requires the typed outer request transition",
                ));
            }
            intent
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid OCOMP intent: {error}")))?;
            receipt
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid OCOMP request receipt: {error}")))?;
            let wwd = WorldwideDay::new(intent.wwd);
            if WwdStatus::try_from(self.worldwide_days.entry(wwd).status().read()?)?
                != WwdStatus::Ready
            {
                return Err(fatal("OCOMP request requires READY WorldwideDay"));
            }

            let receipt_hash = receipt
                .receipt_hash(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP request receipt: {error}")))?;
            if receipt_hash
                != intent
                    .frozen_metadosis_values
                    .request_budget_split_receipt_hash
                || receipt.wwd != intent.wwd
                || receipt.pending_nonce > intent.pending_nonce
                || receipt.protocol_bundle_hash != intent.protocol_bundle_hash
                || receipt.lysis_budget != intent.frozen_metadosis_values.lysis_budget
            {
                return Err(fatal("OCOMP intent/request receipt binding mismatch"));
            }
            if decode_live_scheduler_index(&self.ocomp_scheduler.read()?)?.len() >= MAX_LIVE_JOBS {
                return Err(fatal("OCOMP live Job capacity exhausted"));
            }
            let mut state = self.ocomp_fsm_state(wwd, schema_limits, fsm_limits)?;
            let ready_key = ReadyIndexKey::from_projection(state.projection())?;
            let existing_receipt = self.request_budget_receipt(wwd, schema_limits)?;
            if matches!(existing_receipt, Some(ref existing) if existing != receipt) {
                return Err(fatal("immutable OCOMP request receipt changed"));
            }
            let intent_id = intent
                .intent_id(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP intent: {error}")))?;
            let storage_key = intent_storage_key(intent_id)
                .map_err(|error| fatal(format!("derive OCOMP intent storage key: {error}")))?;
            if !self.ocomp_job_records.get_bytes(&storage_key).is_empty()? {
                return Err(fatal("OCOMP IntentId already has a job record"));
            }
            state
                .apply(
                    JobFsmCommand::Request {
                        at_height: intent.logical_evaluation_height,
                        intent_id,
                        lysis_budget: intent.frozen_metadosis_values.lysis_budget,
                        request_budget_receipt_hash: receipt_hash,
                    },
                    fsm_limits,
                )
                .map_err(|error| fatal(error.to_string()))?;

            if existing_receipt.is_none() {
                self.ocomp_request_budget_receipts.get_bytes(&wwd).write(
                    &receipt
                        .encode_canonical(schema_limits)
                        .map_err(|error| fatal(format!("encode request receipt: {error}")))?,
                )?;
            }
            let record = OcompJobRecordV1 {
                intent: intent.clone(),
                intent_height: intent.logical_evaluation_height,
                status: OcompJobStatus::AwaitingFinality,
                finalized: None,
                terminal: None,
            };
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            commit_outer_transition(
                self,
                wwd,
                outer_transition,
                intent.logical_evaluation_height,
            )?;
            let mut ready_index = self.read_ready_index()?;
            remove_ready_key(&mut ready_index, ready_key)?;
            self.write_ready_index(&ready_index)?;
            self.write_ocomp_state(&state)?;
            self.write_live_scheduler(&state)
        })()
    }

    /// Records the consensus-certified request block and derives the voting
    /// window. The request itself carries no deadline.
    pub fn record_ocomp_finality(
        &mut self,
        intent_id: B256,
        finalized_request_block_hash: B256,
        finalized_request_state_root: B256,
        finality_recorded_height: u64,
        response_window_blocks: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<OcompFinalizedJobV1> {
        (|| {
            let mut record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP finality record has no matching intent"))?;
            if record.status != OcompJobStatus::AwaitingFinality || record.terminal.is_some() {
                return Err(fatal("OCOMP finality requires AWAITING_FINALITY"));
            }
            if finality_recorded_height < record.intent_height || response_window_blocks == 0 {
                return Err(fatal("OCOMP finality/window height is invalid"));
            }
            let open_height = finality_recorded_height
                .checked_add(RESULT_VOTE_MIN_FINALITY_DEPTH)
                .ok_or_else(|| fatal("OCOMP voting open height overflow"))?;
            let deadline_height = open_height
                .checked_add(response_window_blocks)
                .ok_or_else(|| fatal("OCOMP response deadline overflow"))?;
            let finalized = OcompFinalizedJobV1 {
                job_id: record
                    .intent
                    .job_id(
                        finalized_request_block_hash,
                        finalized_request_state_root,
                        schema_limits,
                    )
                    .map_err(|error| fatal(format!("derive finalized OCOMP JobId: {error}")))?,
                finalized_request_block_hash,
                finalized_request_state_root,
                finality_recorded_height,
                open_height,
                deadline_height,
                quorum: None,
            };
            finalized
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid finalized OCOMP job: {error}")))?;
            if let Some(existing) = &record.finalized {
                if existing == &finalized {
                    return Ok(existing.clone());
                }
                return Err(fatal("OCOMP finality binding changed"));
            }
            record.finalized = Some(finalized.clone());
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            Ok(finalized)
        })()
    }

    /// Opens the exact finalized job at `finality_recorded_height + 4`.
    /// Returns `false` before the due height and fails if consensus skipped it.
    pub(crate) fn open_due_ocomp_voting(
        &mut self,
        at_height: u64,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<bool> {
        (|| {
            let mut selected = None;
            for state in self.live_ocomp_fsm_states(schema_limits, fsm_limits)? {
                let intent_id = state
                    .projection()
                    .live_intent_id
                    .ok_or_else(|| fatal("OCOMP live scheduler has no IntentId"))?;
                let record = self
                    .ocomp_job_record(intent_id, schema_limits)?
                    .ok_or_else(|| fatal("OCOMP live scheduler job is missing"))?;
                if record.status != OcompJobStatus::AwaitingFinality {
                    continue;
                }
                let Some(finalized) = record.finalized.clone() else {
                    continue;
                };
                if at_height < finalized.open_height {
                    continue;
                }
                if at_height > finalized.open_height {
                    return Err(fatal(
                        "OCOMP consensus skipped the exact voting-open height",
                    ));
                }
                if selected
                    .replace((state, intent_id, record, finalized))
                    .is_some()
                {
                    return Err(fatal(
                        "multiple OCOMP jobs are due at one bounded open height",
                    ));
                }
            }
            let Some((mut state, intent_id, mut record, finalized)) = selected else {
                return Ok(false);
            };
            state
                .apply(
                    JobFsmCommand::OpenVoting {
                        at_height,
                        deadline_height: finalized.deadline_height,
                    },
                    fsm_limits,
                )
                .map_err(|error| fatal(error.to_string()))?;
            let accountability = OcompVoteAccountabilityV1::empty(
                finalized.job_id,
                record.intent.result_validator_set_epoch,
                record.intent.result_committee_set_hash,
                record.intent.result_ocomp_binding_hash,
                record.intent.result_member_count,
                record.intent.result_quorum_threshold,
            )
            .map_err(|error| fatal(format!("create OCOMP vote slots: {error}")))?;
            let slot = self.ocomp_vote_accountability.get_bytes(&finalized.job_id);
            if !slot.is_empty()? {
                return Err(fatal("OCOMP vote accountability already exists"));
            }
            slot.write(
                &accountability
                    .encode_canonical(schema_limits)
                    .map_err(|error| fatal(format!("encode OCOMP vote slots: {error}")))?,
            )?;
            let mut response_index = self.read_response_deadline_index()?;
            insert_response_deadline_key(
                &mut response_index,
                ResponseDeadlineKey {
                    deadline_height: finalized.deadline_height,
                    job_id: finalized.job_id,
                    intent_id,
                },
            )?;
            record.status = OcompJobStatus::VotingOpen;
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.write_ocomp_state(&state)?;
            self.write_live_scheduler(&state)?;
            self.write_response_deadline_index(&response_index)?;
            Ok(true)
        })()
    }

    /// Applies the exclusive begin-zone expiry to the exact live index.
    ///
    /// The explicit arguments are the complete authorization, outer-FSM, time,
    /// schema, and inner-FSM boundary for one atomic expiry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn expire_ocomp_job(
        &mut self,
        outer_transition: &OuterWwdTransition,
        intent_id: B256,
        at_height: u64,
        at_time: u64,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<OcompExpiryDisposition> {
        (|| {
            let mut state = self
                .live_ocomp_fsm_state_by_intent(intent_id, schema_limits, fsm_limits)?
                .ok_or_else(|| fatal("OCOMP expiry index is empty"))?;
            let live_intent_id = state
                .projection()
                .live_intent_id
                .ok_or_else(|| fatal("OCOMP expiry index has no live intent"))?;
            if live_intent_id != intent_id {
                return Err(fatal("OCOMP expiry selected a different live job"));
            }
            let mut record = self
                .ocomp_job_record(live_intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP live index points to a missing job"))?;
            let wwd = WorldwideDay::new(record.intent.wwd);
            let before_terminal = state.projection().terminal_records;
            state
                .apply(JobFsmCommand::Expire { at_height, at_time }, fsm_limits)
                .map_err(|error| fatal(error.to_string()))?;
            let terminal = state
                .terminal_attempts()
                .last()
                .copied()
                .ok_or_else(|| fatal("OCOMP expiry produced no terminal evidence"))?;
            if terminal.intent_id != live_intent_id
                || terminal.outcome != super::state::RetryTerminalOutcome::Expired
                || state.projection().terminal_records != before_terminal.saturating_add(1)
            {
                return Err(fatal("OCOMP terminal index/count mismatch"));
            }
            // Lockstep: the persisted index and the FSM snapshot are written
            // separately; the day's indexed count must equal the FSM's
            // pre-transition terminal count before this push extends either.
            let indexed_terminal = self.terminal_intent_count(wwd)?;
            if indexed_terminal != before_terminal {
                return Err(fatal(
                    "OCOMP terminal index diverged from the FSM terminal count",
                ));
            }
            if indexed_terminal >= fsm_limits.max_terminal_records {
                return Err(fatal("OCOMP terminal record cap exhausted"));
            }
            let projection = state.projection();
            let next_pending_nonce = terminal.next_pending_nonce;
            let retained_lysis_budget = projection
                .retained_lysis_budget
                .ok_or_else(|| fatal("expired OCOMP job has no retained Lysis budget"))?;
            if retained_lysis_budget != record.intent.frozen_metadosis_values.lysis_budget {
                return Err(fatal("expired OCOMP job retained budget mismatch"));
            }

            record.status = OcompJobStatus::Expired;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Expired,
                terminal_height: terminal.terminal_height,
                terminal_time: terminal.terminal_time,
                next_pending_nonce: Some(terminal.next_pending_nonce),
                completed_binding: None,
            });
            self.write_ocomp_job_record(live_intent_id, &record, schema_limits)?;
            self.push_terminal_intent(wwd, live_intent_id, fsm_limits.max_terminal_records)?;

            if projection.terminal_records == fsm_limits.max_terminal_records {
                if !matches!(
                    outer_transition.kind(),
                    OuterWwdTransitionKind::OcompAttemptsExhausted
                ) {
                    return Err(fatal(
                        "terminal OCOMP expiry requires the attempts-exhausted outer transition",
                    ));
                }
                if self
                    .read_ready_index()?
                    .iter()
                    .any(|key| key.worldwide_day == wwd)
                {
                    return Err(fatal(
                        "terminal no-retry WWD unexpectedly remains in READY index",
                    ));
                }
                let carry_over = PromisLimitContract::new(self.storage.clone())
                    .checked_add_carry_over(retained_lysis_budget)?;
                commit_outer_transition(self, wwd, outer_transition, at_height)?;
                self.ocomp_fsm_states.get_bytes(&wwd).clear()?;
                self.remove_live_scheduler(live_intent_id)?;
                return Ok(OcompExpiryDisposition::TerminalNoRetry {
                    next_pending_nonce,
                    carry_over,
                });
            }

            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompRetryScheduled(OcompRetryCause::Expired)
            ) {
                return Err(fatal(
                    "retryable OCOMP expiry requires the typed expired-retry outer transition",
                ));
            }
            commit_outer_transition(self, wwd, outer_transition, at_height)?;
            let ready_key = ReadyIndexKey::from_projection(projection)?;
            let mut ready_index = self.read_ready_index()?;
            insert_ready_key(&mut ready_index, ready_key)?;
            self.write_ocomp_state(&state)?;
            self.write_ready_index(&ready_index)?;
            self.remove_live_scheduler(live_intent_id)?;
            Ok(OcompExpiryDisposition::RetryScheduled { next_pending_nonce })
        })()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_ocomp_conflict(
        &mut self,
        outer_transition: &OuterWwdTransition,
        intent_id: B256,
        completed_binding: OcompCompletedBindingV1,
        quorum: &outbe_ocomp_protocol::vote::OcompQuorumV1,
        at_height: u64,
        at_time: u64,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<u64> {
        (|| {
            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompRetryScheduled(OcompRetryCause::Conflicted)
            ) {
                return Err(fatal(
                    "OCOMP conflict requires the typed conflicted-retry outer transition",
                ));
            }
            let mut state = self
                .live_ocomp_fsm_state_by_intent(intent_id, schema_limits, fsm_limits)?
                .ok_or_else(|| fatal("OCOMP conflict has no live job"))?;
            if state.projection().live_intent_id != Some(intent_id) {
                return Err(fatal("OCOMP conflict IntentId is not the live job"));
            }
            let mut record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP conflict job record is missing"))?;
            let wwd = WorldwideDay::new(record.intent.wwd);
            if record.status != OcompJobStatus::VotingOpen || record.terminal.is_some() {
                return Err(fatal("OCOMP conflict requires a voting-open job"));
            }
            if record
                .finalized
                .as_ref()
                .and_then(|finalized| finalized.quorum.as_ref())
                .is_some()
            {
                return Err(fatal("OCOMP conflict job already has a quorum"));
            }

            completed_binding
                .validate_semantics(quorum, schema_limits)
                .map_err(|error| fatal(format!("invalid OCOMP conflict binding: {error}")))?;
            let receipt = &completed_binding.terminal_receipt;
            let activation_preconditions_hash = record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP activation preconditions: {error}")))?;
            if receipt.outcome != ActivationOutcome::ConflictResolved
                || receipt.binding.intent_id != intent_id
                || receipt.binding.attempt != record.intent.attempt
                || receipt.binding.protocol_bundle_hash != record.intent.protocol_bundle_hash
                || receipt.binding.activation_preconditions_hash != activation_preconditions_hash
                || receipt.request_budget_split_receipt_hash
                    != record
                        .intent
                        .frozen_metadosis_values
                        .request_budget_split_receipt_hash
                || receipt.activated_at_height != at_height
                || receipt.activated_at_time != at_time
            {
                return Err(fatal("OCOMP conflict receipt is not bound to the live job"));
            }

            let before_terminal = state.projection().terminal_records;
            state
                .apply(JobFsmCommand::Conflict { at_height, at_time }, fsm_limits)
                .map_err(|error| fatal(error.to_string()))?;
            let terminal = state
                .terminal_attempts()
                .last()
                .copied()
                .ok_or_else(|| fatal("OCOMP conflict produced no terminal evidence"))?;
            if terminal.intent_id != intent_id
                || terminal.outcome != RetryTerminalOutcome::Conflicted
                || state.projection().terminal_records != before_terminal.saturating_add(1)
            {
                return Err(fatal("OCOMP conflict terminal index/count mismatch"));
            }
            // Lockstep with the persisted per-day index; see `expire_ocomp_job`.
            let indexed_terminal = self.terminal_intent_count(wwd)?;
            if indexed_terminal != before_terminal {
                return Err(fatal(
                    "OCOMP terminal index diverged from the FSM terminal count",
                ));
            }
            if indexed_terminal >= fsm_limits.max_terminal_records {
                return Err(fatal("OCOMP terminal record cap exhausted"));
            }

            let projection = state.projection();
            record
                .finalized
                .as_mut()
                .ok_or_else(|| fatal("OCOMP conflict job is not finalized"))?
                .quorum = Some(quorum.clone());
            record.status = OcompJobStatus::Conflicted;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Conflicted,
                terminal_height: at_height,
                terminal_time: at_time,
                next_pending_nonce: Some(terminal.next_pending_nonce),
                completed_binding: Some(completed_binding),
            });
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.push_terminal_intent(wwd, intent_id, fsm_limits.max_terminal_records)?;

            commit_outer_transition(self, wwd, outer_transition, at_height)?;
            let ready_key = ReadyIndexKey::from_projection(projection)?;
            let mut ready_index = self.read_ready_index()?;
            insert_ready_key(&mut ready_index, ready_key)?;
            self.write_ocomp_state(&state)?;
            self.write_ready_index(&ready_index)?;
            self.remove_live_scheduler(intent_id)?;
            Ok(terminal.next_pending_nonce)
        })()
    }

    /// Commits the certified terminal receipt and active generation after all
    /// four owner receipts have been verified in the same activation frame.
    ///
    /// The one-shot terminal permit is advanced only after every consensus
    /// write and event succeeds.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_ocomp_completed(
        &mut self,
        outer_transition: &OuterWwdTransition,
        intent_id: B256,
        active_generation: ActiveGenerationV1,
        result_evidence_hash: B256,
        nod_gratis_consumed: U256,
        unused_lysis: U256,
        activated_at_height: u64,
        activated_at_time: u64,
        permit: LysisTerminalPermitV1<'_, '_>,
        quorum: &outbe_ocomp_protocol::vote::OcompQuorumV1,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<OcompCompletedBindingV1> {
        (|| {
            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompCompleted
            ) {
                return Err(fatal(
                    "OCOMP completion requires the typed completed outer transition",
                ));
            }
            let state = self
                .live_ocomp_fsm_state_by_intent(intent_id, schema_limits, fsm_limits)?
                .ok_or_else(|| fatal("OCOMP completion has no live job"))?;
            let projection = state.projection();
            if projection.live_intent_id != Some(intent_id) {
                return Err(fatal("OCOMP completion IntentId is not the live job"));
            }
            let mut record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP completion job record is missing"))?;
            if record.status != OcompJobStatus::VotingOpen || record.terminal.is_some() {
                return Err(fatal("OCOMP completion requires a voting-open job"));
            }
            if record
                .finalized
                .as_ref()
                .and_then(|finalized| finalized.quorum.as_ref())
                .is_some()
            {
                return Err(fatal("OCOMP completion job already has a quorum"));
            }
            // Lockstep with the persisted per-day index; see `expire_ocomp_job`.
            let wwd = WorldwideDay::new(record.intent.wwd);
            let indexed_terminal = self.terminal_intent_count(wwd)?;
            if indexed_terminal != projection.terminal_records {
                return Err(fatal(
                    "OCOMP terminal index diverged from the FSM terminal count",
                ));
            }
            if indexed_terminal >= fsm_limits.max_terminal_records {
                return Err(fatal("OCOMP terminal record cap exhausted"));
            }

            let activation_preconditions_hash = record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP activation preconditions: {error}")))?;
            let binding = permit.binding().clone();
            if binding.intent_id != intent_id
                || binding.job_id != active_generation.job_id
                || binding.attempt != record.intent.attempt
                || binding.protocol_bundle_hash != record.intent.protocol_bundle_hash
                || binding.activation_preconditions_hash != activation_preconditions_hash
                || permit.request_budget_split_receipt_hash()
                    != record
                        .intent
                        .frozen_metadosis_values
                        .request_budget_split_receipt_hash
                || unused_lysis > record.intent.frozen_metadosis_values.lysis_budget
                || nod_gratis_consumed.checked_add(unused_lysis)
                    != Some(record.intent.frozen_metadosis_values.lysis_budget)
            {
                return Err(fatal("OCOMP terminal permit is not bound to the live job"));
            }
            if result_evidence_hash.is_zero() {
                return Err(fatal("OCOMP result evidence hash is zero"));
            }
            if active_generation.result_evidence_hash != result_evidence_hash {
                return Err(fatal(
                    "active Lysis generation differs from result evidence",
                ));
            }

            if self.active_lysis_generation(wwd, schema_limits)?.is_some() {
                return Err(fatal("active Lysis generation cannot be overwritten"));
            }
            let active_generation_hash = active_generation
                .active_generation_hash(schema_limits)
                .map_err(|error| fatal(format!("hash active Lysis generation: {error}")))?;
            let receipt = AggregateActivationReceiptV1 {
                binding: binding.clone(),
                outcome: ActivationOutcome::Applied,
                nod_receipt_hash: Some(permit.nod_receipt_hash()),
                contributor_receipt_hash: Some(permit.contributor_receipt_hash()),
                tribute_receipt_hash: Some(permit.tribute_receipt_hash()),
                carry_over_receipt_hash: Some(permit.carry_over_receipt_hash()),
                request_budget_split_receipt_hash: permit.request_budget_split_receipt_hash(),
                active_generation_hash: Some(active_generation_hash),
                effect_commitment: permit.effect_commitment(),
                event_summary_hash: permit.event_summary_hash(),
                activated_at_height,
                activated_at_time,
            };
            receipt
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid applied terminal receipt: {error}")))?;
            let terminal_receipt_hash = receipt
                .terminal_receipt_hash(schema_limits)
                .map_err(|error| fatal(format!("hash applied terminal receipt: {error}")))?;
            let completed_binding = OcompCompletedBindingV1 {
                job_id: binding.job_id,
                activation_call_id: permit.activation_call_id(),
                result_digest: binding.result_digest,
                quorum_height: quorum.quorum_height,
                quorum_signer_bitmap: quorum.signer_bitmap.clone(),
                quorum_evidence_hash: quorum.evidence_hash,
                result_evidence_hash,
                terminal_receipt_hash,
                terminal_receipt: receipt,
            };
            completed_binding
                .validate_semantics(quorum, schema_limits)
                .map_err(|error| fatal(format!("invalid OCOMP completed binding: {error}")))?;

            self.ocomp_active_lysis_generations.get_bytes(&wwd).write(
                &active_generation
                    .encode_canonical(schema_limits)
                    .map_err(|error| fatal(format!("encode active Lysis generation: {error}")))?,
            )?;
            record
                .finalized
                .as_mut()
                .ok_or_else(|| fatal("OCOMP completion job is not finalized"))?
                .quorum = Some(quorum.clone());
            record.status = OcompJobStatus::Completed;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Completed,
                terminal_height: activated_at_height,
                terminal_time: activated_at_time,
                next_pending_nonce: None,
                completed_binding: Some(completed_binding.clone()),
            });
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.push_terminal_intent(wwd, intent_id, fsm_limits.max_terminal_records)?;
            commit_outer_transition(self, wwd, outer_transition, activated_at_height)?;
            self.remove_live_scheduler(intent_id)?;
            self.ocomp_fsm_states.get_bytes(&wwd).clear()?;

            let frozen = &record.intent.frozen_metadosis_values;
            self.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: record.intent.wwd,
                tributeTotals: record.intent.authenticated_day_nominal,
                dayGratisDemand: frozen.gratis_demand,
                dayGratisLimit: frozen.gratis_supply,
                dayGratisAllocation: frozen.lysis_budget,
                dayGratisAllocationRemainder: unused_lysis,
                netDayGratisAllocation: nod_gratis_consumed,
                dayMetadosisLimitRemainder: unused_lysis,
                status: "COMPLETED".into(),
                blockNumber: activated_at_height,
            })?;
            self.emit(IMetadosis::LysisActivated {
                intentId: intent_id,
                jobId: binding.job_id,
                activationCallId: permit.activation_call_id(),
                resultDigest: binding.result_digest,
                terminalReceiptHash: terminal_receipt_hash,
                wwd: record.intent.wwd,
            })?;
            permit
                .commit_terminal()
                .map_err(|error| fatal(format!("commit Lysis terminal permit: {error}")))?;
            Ok(completed_binding)
        })()
    }
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
