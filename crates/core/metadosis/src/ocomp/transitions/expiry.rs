use super::super::state::JobFsmCommand;
use crate::{
    errors::storage_corruption_message,
    reducer::{OuterWwdTransition, OuterWwdTransitionKind},
    schema::MetadosisContract,
};
use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    state::{LysisTerminalV1, OcompJobStatus, OcompTerminalOutcome},
    SchemaLimits,
};
use outbe_primitives::{error::Result, time::WorldwideDay};
impl MetadosisContract<'_> {
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
        let expiry = ExpiryAuthorization {
            outer_transition,
            intent_id,
            at_height,
            at_time,
            schema_limits,
        };
        let PreparedExpiry {
            wwd,
            live_intent_id,
            mut record,
            terminal,
        } = self.prepare_expiry(&expiry)?;
        let retained_lysis_limit_minor = terminal.retained_lysis_limit_minor;
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
    }
    fn prepare_expiry(&self, expiry: &ExpiryAuthorization<'_>) -> Result<PreparedExpiry> {
        let ExpiryAuthorization {
            outer_transition,
            intent_id,
            schema_limits,
            ..
        } = *expiry;
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
        let live_intent_id = state
            .projection()
            .live_intent_id
            .ok_or_else(|| storage_corruption_message("OCOMP expiry index has no live intent"))?;
        if live_intent_id != intent_id {
            return Err(storage_corruption_message(
                "OCOMP expiry selected a different live job",
            ));
        }
        let record = self
            .ocomp_job_record(live_intent_id, schema_limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP live index points to a missing job")
            })?;
        let wwd = WorldwideDay::new(record.intent.wwd);
        let terminal = apply_expiry_evidence(&mut state, &record, expiry)?;
        let indexed_terminal = self.terminal_intent_count(wwd)?;
        if indexed_terminal != 0 {
            return Err(storage_corruption_message(
                "OCOMP WorldwideDay already has a terminal job",
            ));
        }
        let retained_lysis_limit_minor = terminal.retained_lysis_limit_minor;
        if retained_lysis_limit_minor != record.intent.frozen_metadosis_values.lysis_limit_minor {
            return Err(storage_corruption_message(
                "expired OCOMP job retained limit mismatch",
            ));
        }

        Ok(PreparedExpiry {
            wwd,
            live_intent_id,
            record,
            terminal,
        })
    }
}

struct ExpiryAuthorization<'attempt> {
    outer_transition: &'attempt OuterWwdTransition,
    intent_id: B256,
    at_height: u64,
    at_time: u64,
    schema_limits: &'attempt SchemaLimits,
}

struct PreparedExpiry {
    wwd: WorldwideDay,
    live_intent_id: B256,
    record: outbe_ocomp_protocol::state::OcompJobRecordV1,
    terminal: super::super::state::TerminalAttempt,
}

fn apply_expiry_evidence(
    state: &mut super::super::state::JobFsmState,
    record: &outbe_ocomp_protocol::state::OcompJobRecordV1,
    expiry: &ExpiryAuthorization<'_>,
) -> Result<super::super::state::TerminalAttempt> {
    let at_height = expiry.at_height;
    let at_time = expiry.at_time;
    let live_intent_id = expiry.intent_id;
    state
        .apply(JobFsmCommand::Expire { at_height, at_time })
        .map_err(|error| storage_corruption_message(error.to_string()))?;
    let terminal =
        state.terminal_attempts().last().copied().ok_or_else(|| {
            storage_corruption_message("OCOMP expiry produced no terminal evidence")
        })?;
    if terminal.intent_id != live_intent_id
        || terminal.pending_nonce != record.intent.pending_nonce
        || state.projection().phase != super::super::state::DayPhase::Terminal
    {
        return Err(storage_corruption_message(
            "OCOMP terminal evidence is inconsistent",
        ));
    }
    Ok(terminal)
}
