mod finality;
mod request;
mod voting;

use alloy_primitives::{B256, U256};
use outbe_lysis::activation_v1::LysisTerminalPermitV1;
use outbe_ocomp_protocol::{
    receipts::{ActivationOutcome, AggregateActivationReceiptV1},
    state::{
        ActiveGenerationV1, LysisTerminalV1, OcompCompletedBindingV1, OcompJobStatus,
        OcompTerminalOutcome,
    },
    SchemaLimits,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::{
    commit::commit_outer_transition,
    errors::storage_corruption_message,
    precompile::IMetadosis,
    reducer::{OuterWwdTransition, OuterWwdTransitionKind},
    schema::MetadosisContract,
};

use super::{
    index::{remove_ready_key, ReadyIndexKey},
    state::JobFsmCommand,
};

impl MetadosisContract<'_> {
    /// Removes a READY scheduler entry when Metadosis fails before committing
    /// a canonical OCOMP job.
    ///
    /// Once a job exists, only its own deadline may make it terminal. In
    /// particular, this outer failure path must never construct the reserved
    /// `Failed` OCOMP outcome.
    pub(crate) fn clear_ready_ocomp_for_failed_day(
        &mut self,
        wwd: WorldwideDay,
        schema_limits: &SchemaLimits,
    ) -> Result<()> {
        if self.ocomp_fsm_states.get_bytes(&wwd).is_empty()? {
            return Ok(());
        }
        let state = self.ocomp_fsm_state(wwd, schema_limits)?;
        let projection = state.projection();
        if projection.live_intent_id.is_some() {
            return Err(storage_corruption_message(
                "Metadosis failure cannot replace a live canonical OCOMP job",
            ));
        }
        let ready_key = ReadyIndexKey::from_projection(projection)?;
        let mut ready_index = self.read_ready_index()?;
        remove_ready_key(&mut ready_index, ready_key)?;
        self.write_ready_index(&ready_index)?;
        self.ocomp_fsm_states.get_bytes(&wwd).clear()
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
    ) -> Result<U256> {
        (|| {
            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompExpired
            ) {
                return Err(storage_corruption_message(
                    "OCOMP expiry requires the typed outer expiry transition",
                ));
            }
            let mut state = self
                .live_ocomp_fsm_state_by_intent(intent_id, schema_limits)?
                .ok_or_else(|| storage_corruption_message("OCOMP expiry index is empty"))?;
            let live_intent_id = state.projection().live_intent_id.ok_or_else(|| {
                storage_corruption_message("OCOMP expiry index has no live intent")
            })?;
            if live_intent_id != intent_id {
                return Err(storage_corruption_message(
                    "OCOMP expiry selected a different live job",
                ));
            }
            let mut record = self
                .ocomp_job_record(live_intent_id, schema_limits)?
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP live index points to a missing job")
                })?;
            let wwd = WorldwideDay::new(record.intent.wwd);
            state
                .apply(JobFsmCommand::Expire { at_height, at_time })
                .map_err(|error| storage_corruption_message(error.to_string()))?;
            let terminal = state.terminal_attempts().last().copied().ok_or_else(|| {
                storage_corruption_message("OCOMP expiry produced no terminal evidence")
            })?;
            if terminal.intent_id != live_intent_id
                || terminal.pending_nonce != record.intent.pending_nonce
                || state.projection().phase != super::state::DayPhase::Terminal
            {
                return Err(storage_corruption_message(
                    "OCOMP terminal evidence is inconsistent",
                ));
            }
            let indexed_terminal = self.terminal_intent_count(wwd)?;
            if indexed_terminal != 0 {
                return Err(storage_corruption_message(
                    "OCOMP WorldwideDay already has a terminal job",
                ));
            }
            let retained_lysis_limit_minor = terminal.retained_lysis_limit_minor;
            if retained_lysis_limit_minor != record.intent.frozen_metadosis_values.lysis_limit_minor
            {
                return Err(storage_corruption_message(
                    "expired OCOMP job retained limit mismatch",
                ));
            }

            record.status = OcompJobStatus::Expired;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Expired,
                terminal_height: terminal.terminal_height,
                terminal_time: terminal.terminal_time,
                completed_binding: None,
            });
            self.write_ocomp_job_record(live_intent_id, &record, schema_limits)?;
            self.push_terminal_intent(wwd, live_intent_id)?;
            self.release_ocomp_lineage(live_intent_id, at_height, schema_limits)?;
            Ok(retained_lysis_limit_minor)
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
        lysis_allocation_minor: U256,
        unused_lysis_limit_minor: U256,
        activated_at_height: u64,
        activated_at_time: u64,
        permit: LysisTerminalPermitV1<'_, '_>,
        quorum: &outbe_ocomp_protocol::vote::OcompQuorumV1,
        schema_limits: &SchemaLimits,
    ) -> Result<OcompCompletedBindingV1> {
        (|| {
            if !matches!(
                outer_transition.kind(),
                OuterWwdTransitionKind::OcompCompleted
            ) {
                return Err(storage_corruption_message(
                    "OCOMP completion requires the typed completed outer transition",
                ));
            }
            let state = self
                .live_ocomp_fsm_state_by_intent(intent_id, schema_limits)?
                .ok_or_else(|| storage_corruption_message("OCOMP completion has no live job"))?;
            let projection = state.projection();
            if projection.live_intent_id != Some(intent_id) {
                return Err(storage_corruption_message(
                    "OCOMP completion IntentId is not the live job",
                ));
            }
            let mut record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP completion job record is missing")
                })?;
            if record.status != OcompJobStatus::VotingOpen || record.terminal.is_some() {
                return Err(storage_corruption_message(
                    "OCOMP completion requires a voting-open job",
                ));
            }
            if record
                .finalized
                .as_ref()
                .and_then(|finalized| finalized.quorum.as_ref())
                .is_some()
            {
                return Err(storage_corruption_message(
                    "OCOMP completion job already has a quorum",
                ));
            }
            // Lockstep with the persisted per-day index; see `expire_ocomp_job`.
            let wwd = WorldwideDay::new(record.intent.wwd);
            let indexed_terminal = self.terminal_intent_count(wwd)?;
            if indexed_terminal != 0 || projection.terminal_records != 0 {
                return Err(storage_corruption_message(
                    "OCOMP WorldwideDay already has a terminal job",
                ));
            }

            let activation_preconditions_hash = record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(schema_limits)
                .map_err(|error| {
                    storage_corruption_message(format!(
                        "hash OCOMP activation preconditions: {error}"
                    ))
                })?;
            let binding = permit.binding().clone();
            if binding.intent_id != intent_id
                || binding.job_id != active_generation.job_id
                || binding.attempt != record.intent.attempt
                || binding.protocol_bundle_hash != record.intent.protocol_bundle_hash
                || binding.activation_preconditions_hash != activation_preconditions_hash
                || permit.request_limit_split_receipt_hash()
                    != record
                        .intent
                        .frozen_metadosis_values
                        .request_limit_split_receipt_hash
                || unused_lysis_limit_minor
                    > record.intent.frozen_metadosis_values.lysis_limit_minor
                || lysis_allocation_minor.checked_add(unused_lysis_limit_minor)
                    != Some(record.intent.frozen_metadosis_values.lysis_limit_minor)
            {
                return Err(storage_corruption_message(
                    "OCOMP terminal permit is not bound to the live job",
                ));
            }
            if result_evidence_hash.is_zero() {
                return Err(storage_corruption_message(
                    "OCOMP result evidence hash is zero",
                ));
            }
            if active_generation.result_evidence_hash != result_evidence_hash {
                return Err(storage_corruption_message(
                    "active Lysis generation differs from result evidence",
                ));
            }

            if self.active_lysis_generation(wwd, schema_limits)?.is_some() {
                return Err(storage_corruption_message(
                    "active Lysis generation cannot be overwritten",
                ));
            }
            let active_generation_hash = active_generation
                .active_generation_hash(schema_limits)
                .map_err(|error| {
                    storage_corruption_message(format!("hash active Lysis generation: {error}"))
                })?;
            let receipt = AggregateActivationReceiptV1 {
                binding: binding.clone(),
                outcome: ActivationOutcome::Applied,
                nod_receipt_hash: Some(permit.nod_receipt_hash()),
                contributor_receipt_hash: Some(permit.contributor_receipt_hash()),
                tribute_receipt_hash: Some(permit.tribute_receipt_hash()),
                carry_over_receipt_hash: Some(permit.carry_over_receipt_hash()),
                request_limit_split_receipt_hash: permit.request_limit_split_receipt_hash(),
                active_generation_hash: Some(active_generation_hash),
                effect_commitment: permit.effect_commitment(),
                event_summary_hash: permit.event_summary_hash(),
                activated_at_height,
                activated_at_time,
            };
            receipt.validate_semantics().map_err(|error| {
                storage_corruption_message(format!("invalid applied terminal receipt: {error}"))
            })?;
            let terminal_receipt_hash =
                receipt
                    .terminal_receipt_hash(schema_limits)
                    .map_err(|error| {
                        storage_corruption_message(format!(
                            "hash applied terminal receipt: {error}"
                        ))
                    })?;
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
                .map_err(|error| {
                    storage_corruption_message(format!("invalid OCOMP completed binding: {error}"))
                })?;

            self.ocomp_active_lysis_generations.get_bytes(&wwd).write(
                &active_generation
                    .encode_canonical(schema_limits)
                    .map_err(|error| {
                        storage_corruption_message(format!(
                            "encode active Lysis generation: {error}"
                        ))
                    })?,
            )?;
            record
                .finalized
                .as_mut()
                .ok_or_else(|| storage_corruption_message("OCOMP completion job is not finalized"))?
                .quorum = Some(quorum.clone());
            record.status = OcompJobStatus::Completed;
            record.terminal = Some(LysisTerminalV1 {
                outcome: OcompTerminalOutcome::Completed,
                terminal_height: activated_at_height,
                terminal_time: activated_at_time,
                completed_binding: Some(completed_binding.clone()),
            });
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.push_terminal_intent(wwd, intent_id)?;
            commit_outer_transition(self, wwd, outer_transition, activated_at_height)?;
            self.remove_live_scheduler(intent_id)?;
            self.ocomp_fsm_states.get_bytes(&wwd).clear()?;
            self.release_ocomp_lineage(intent_id, activated_at_height, schema_limits)?;

            let frozen = &record.intent.frozen_metadosis_values;
            self.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: record.intent.wwd,
                tributeTotals: record.intent.authenticated_day_nominal,
                dayGratisDemand: frozen.gratis_demand,
                dayGratisLimit: frozen.day_gratis_limit_minor,
                lysisLimitMinor: frozen.lysis_limit_minor,
                unusedLysisLimitMinor: unused_lysis_limit_minor,
                lysisAllocationMinor: lysis_allocation_minor,
                dayMetadosisLimitRemainder: unused_lysis_limit_minor,
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
            permit.commit_terminal().map_err(|error| {
                storage_corruption_message(format!("commit Lysis terminal permit: {error}"))
            })?;
            Ok(completed_binding)
        })()
    }

    fn release_ocomp_lineage(
        &mut self,
        lineage: B256,
        at_height: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<()> {
        let mut registry = outbe_ocompregistry::OcompRegistry::new(self.storage.clone());
        if registry.active_authority(schema_limits)?.is_none() {
            return Err(storage_corruption_message(
                "terminal OCOMP WWD has no active Registry authority",
            ));
        }
        let released = registry.release_lineage(lineage, at_height, schema_limits)?;
        if !released {
            return Err(storage_corruption_message(
                "terminal OCOMP WWD has no Registry lineage pin",
            ));
        }
        Ok(())
    }
}
