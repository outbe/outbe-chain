use crate::{
    commit::commit_outer_transition,
    errors::storage_corruption_message,
    precompile::IMetadosis,
    reducer::{OuterWwdTransition, OuterWwdTransitionKind},
    schema::MetadosisContract,
};
use alloy_primitives::{B256, U256};
use outbe_lysis::activation_v1::LysisTerminalPermitV1;
use outbe_ocomp_protocol::{
    receipts::{ActivationOutcome, AggregateActivationReceiptV1},
    state::{
        ActiveGenerationV1, LysisTerminalV1, OcompCompletedBindingV1, OcompJobRecordV1,
        OcompJobStatus, OcompTerminalOutcome,
    },
    SchemaLimits,
};
use outbe_primitives::{error::Result, time::WorldwideDay};
struct CompletionInput<'a> {
    outer_transition: &'a OuterWwdTransition,
    intent_id: B256,
    active_generation: ActiveGenerationV1,
    result_evidence_hash: B256,
    lysis_allocation_minor: U256,
    unused_lysis_limit_minor: U256,
    activated_at_height: u64,
    activated_at_time: u64,
    quorum: &'a outbe_ocomp_protocol::vote::OcompQuorumV1,
    schema_limits: &'a SchemaLimits,
}
impl MetadosisContract<'_> {
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
        let input = CompletionInput {
            outer_transition,
            intent_id,
            active_generation,
            result_evidence_hash,
            lysis_allocation_minor,
            unused_lysis_limit_minor,
            activated_at_height,
            activated_at_time,
            quorum,
            schema_limits,
        };
        self.complete_ocomp(input, permit)
    }
    fn complete_ocomp(
        &mut self,
        input: CompletionInput<'_>,
        permit: LysisTerminalPermitV1<'_, '_>,
    ) -> Result<OcompCompletedBindingV1> {
        let mut record = self.completion_prestate(&input)?;
        validate_terminal_binding(&input, &record, &permit)?;
        let active_generation = &input.active_generation;
        let result_evidence_hash = input.result_evidence_hash;
        let schema_limits = input.schema_limits;
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

        let wwd = WorldwideDay::new(record.intent.wwd);
        if self.active_lysis_generation(wwd, schema_limits)?.is_some() {
            return Err(storage_corruption_message(
                "active Lysis generation cannot be overwritten",
            ));
        }
        let completed_binding = completed_binding(&input, &permit)?;
        self.persist_completion(&input, &mut record, &completed_binding)?;
        self.emit_completion(&input, &record, &completed_binding, &permit)?;
        permit.commit_terminal().map_err(|error| {
            storage_corruption_message(format!("commit Lysis terminal permit: {error}"))
        })?;
        Ok(completed_binding)
    }
    fn completion_prestate(&self, input: &CompletionInput<'_>) -> Result<OcompJobRecordV1> {
        let outer_transition = input.outer_transition;
        let intent_id = input.intent_id;
        let schema_limits = input.schema_limits;
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
        let record = self
            .ocomp_job_record(intent_id, schema_limits)?
            .ok_or_else(|| storage_corruption_message("OCOMP completion job record is missing"))?;
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

        Ok(record)
    }
    fn persist_completion(
        &mut self,
        input: &CompletionInput<'_>,
        record: &mut OcompJobRecordV1,
        completed_binding: &OcompCompletedBindingV1,
    ) -> Result<()> {
        let outer_transition = input.outer_transition;
        let intent_id = input.intent_id;
        let active_generation = &input.active_generation;
        let activated_at_height = input.activated_at_height;
        let activated_at_time = input.activated_at_time;
        let quorum = input.quorum;
        let schema_limits = input.schema_limits;
        let wwd = WorldwideDay::new(record.intent.wwd);
        self.ocomp_active_lysis_generations.get_bytes(&wwd).write(
            &active_generation
                .encode_canonical(schema_limits)
                .map_err(|error| {
                    storage_corruption_message(format!("encode active Lysis generation: {error}"))
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
        self.write_ocomp_job_record(intent_id, record, schema_limits)?;
        self.push_terminal_intent(wwd, intent_id)?;
        commit_outer_transition(self, wwd, outer_transition, activated_at_height)?;
        self.remove_live_scheduler(intent_id)?;
        self.ocomp_fsm_states.get_bytes(&wwd).clear()?;
        self.release_ocomp_lineage(intent_id, activated_at_height, schema_limits)?;

        Ok(())
    }
    fn emit_completion(
        &mut self,
        input: &CompletionInput<'_>,
        record: &OcompJobRecordV1,
        completed_binding: &OcompCompletedBindingV1,
        permit: &LysisTerminalPermitV1<'_, '_>,
    ) -> Result<()> {
        let intent_id = input.intent_id;
        let lysis_allocation_minor = input.lysis_allocation_minor;
        let unused_lysis_limit_minor = input.unused_lysis_limit_minor;
        let activated_at_height = input.activated_at_height;
        let binding = &completed_binding.terminal_receipt.binding;
        let terminal_receipt_hash = completed_binding.terminal_receipt_hash;
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
        Ok(())
    }
}
fn validate_terminal_binding(
    input: &CompletionInput<'_>,
    record: &OcompJobRecordV1,
    permit: &LysisTerminalPermitV1<'_, '_>,
) -> Result<()> {
    let intent_id = input.intent_id;
    let active_generation = &input.active_generation;
    let lysis_allocation_minor = input.lysis_allocation_minor;
    let unused_lysis_limit_minor = input.unused_lysis_limit_minor;
    let schema_limits = input.schema_limits;
    let activation_preconditions_hash = record
        .intent
        .activation_preconditions
        .activation_preconditions_hash(schema_limits)
        .map_err(|error| {
            storage_corruption_message(format!("hash OCOMP activation preconditions: {error}"))
        })?;
    let binding = permit.binding().clone();
    let identity_matches = binding.intent_id == intent_id
        && binding.job_id == active_generation.job_id
        && binding.attempt == record.intent.attempt;
    let frozen_request_matches = binding.protocol_bundle_hash == record.intent.protocol_bundle_hash
        && binding.activation_preconditions_hash == activation_preconditions_hash
        && permit.request_limit_split_receipt_hash()
            == record
                .intent
                .frozen_metadosis_values
                .request_limit_split_receipt_hash;
    if !identity_matches
        || !frozen_request_matches
        || !limit_conserved(
            record.intent.frozen_metadosis_values.lysis_limit_minor,
            lysis_allocation_minor,
            unused_lysis_limit_minor,
        )
    {
        return Err(storage_corruption_message(
            "OCOMP terminal permit is not bound to the live job",
        ));
    }
    Ok(())
}
fn completed_binding(
    input: &CompletionInput<'_>,
    permit: &LysisTerminalPermitV1<'_, '_>,
) -> Result<OcompCompletedBindingV1> {
    let active_generation = &input.active_generation;
    let result_evidence_hash = input.result_evidence_hash;
    let activated_at_height = input.activated_at_height;
    let activated_at_time = input.activated_at_time;
    let quorum = input.quorum;
    let schema_limits = input.schema_limits;
    let binding = permit.binding().clone();
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
    let terminal_receipt_hash = receipt
        .terminal_receipt_hash(schema_limits)
        .map_err(|error| {
            storage_corruption_message(format!("hash applied terminal receipt: {error}"))
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

    Ok(completed_binding)
}

fn limit_conserved(limit: U256, allocated: U256, unused: U256) -> bool {
    unused <= limit && allocated.checked_add(unused) == Some(limit)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_conserves_exact_limit_without_saturation() {
        assert!(limit_conserved(
            U256::from(10),
            U256::from(7),
            U256::from(3)
        ));
        assert!(limit_conserved(U256::ZERO, U256::ZERO, U256::ZERO));
        assert!(!limit_conserved(
            U256::from(10),
            U256::from(7),
            U256::from(4)
        ));
        assert!(!limit_conserved(U256::from(10), U256::ZERO, U256::from(11)));
        assert!(!limit_conserved(U256::MAX, U256::MAX, U256::from(1)));
    }
}
