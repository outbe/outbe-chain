use super::WwdProjection;
use crate::{
    errors::storage_corruption_message,
    ocomp::{
        poc_schema_limits,
        state::{DayPhase, OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS},
        ResponseDeadlineKey,
    },
    schema::MetadosisContract,
};
use outbe_ocomp_protocol::{state::OcompJobStatus, SchemaLimits};
use outbe_primitives::{
    error::{PrecompileError, Result},
    time::WorldwideDay,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate_ocomp_index_equivalence(
    contract: &MetadosisContract<'_>,
    records: &BTreeMap<WorldwideDay, WwdProjection>,
    active_set: &BTreeSet<WorldwideDay>,
) -> Result<()> {
    let limits = poc_schema_limits();
    if !has_profile(contract, records, &limits)? {
        return Ok(());
    }
    let pending = pending_days(contract, records, &limits)?;
    let windows = live_windows(contract, active_set, &pending, &limits)?;
    validate_response_windows(contract, windows, &limits)?;
    validate_ready_membership(contract, active_set, &limits)
}

fn has_profile(
    contract: &MetadosisContract<'_>,
    records: &BTreeMap<WorldwideDay, WwdProjection>,
    schema_limits: &SchemaLimits,
) -> Result<bool> {
    let Some(_profile) = contract.read_ocomp_request_profile(schema_limits)? else {
        let indexed_fsm_exists = records.keys().try_fold(false, |found, wwd| {
            Ok::<_, PrecompileError>(found || !contract.ocomp_fsm_states.get_bytes(wwd).is_empty()?)
        })?;
        if indexed_fsm_exists {
            return Err(storage_corruption_message(
                "Metadosis OCOMP state exists without an active profile",
            ));
        }
        if !contract.read_ready_index()?.is_empty()
            || !contract.ocomp_scheduler.is_empty()?
            || !contract.read_response_deadline_index()?.is_empty()
        {
            return Err(storage_corruption_message(
                "Metadosis OCOMP state exists without an active profile",
            ));
        }
        return Ok(false);
    };
    Ok(true)
}

fn pending_days(
    contract: &MetadosisContract<'_>,
    records: &BTreeMap<WorldwideDay, WwdProjection>,
    schema_limits: &SchemaLimits,
) -> Result<BTreeSet<WorldwideDay>> {
    let mut pending_fsm_wwds = BTreeSet::new();
    for projection in records.values() {
        if !contract
            .ocomp_fsm_states
            .get_bytes(&projection.worldwide_day)
            .is_empty()?
        {
            let state = contract.ocomp_fsm_state(projection.worldwide_day, schema_limits)?;
            if state.projection().phase == DayPhase::OffchainPending {
                pending_fsm_wwds.insert(projection.worldwide_day);
            }
        }
    }
    Ok(pending_fsm_wwds)
}

fn live_windows(
    contract: &MetadosisContract<'_>,
    active_set: &BTreeSet<WorldwideDay>,
    pending_fsm_wwds: &BTreeSet<WorldwideDay>,
    schema_limits: &SchemaLimits,
) -> Result<BTreeSet<ResponseDeadlineKey>> {
    let mut unmatched_voting_windows = BTreeSet::new();
    let mut live_scheduler_wwds = BTreeSet::new();
    for live in contract.live_ocomp_fsm_states(schema_limits)? {
        let projection = live.projection();
        if !active_set.contains(&projection.worldwide_day) {
            return Err(storage_corruption_message(
                "OCOMP live scheduler points outside active WWD index",
            ));
        }
        if !live_scheduler_wwds.insert(projection.worldwide_day) {
            return Err(storage_corruption_message(
                "OCOMP live scheduler contains a duplicate WWD",
            ));
        }
        let deadline_height = projection
            .deadline_height
            .ok_or_else(|| storage_corruption_message("OCOMP live FSM has no deadline"))?;
        let intent_id = projection
            .live_intent_id
            .ok_or_else(|| storage_corruption_message("OCOMP live FSM has no live intent"))?;
        let record = contract
            .ocomp_job_record(intent_id, schema_limits)?
            .ok_or_else(|| storage_corruption_message("OCOMP live FSM has no job record"))?;
        if let Some(key) = voting_window(&record, deadline_height, intent_id)? {
            unmatched_voting_windows.insert(key);
        }
    }
    if live_scheduler_wwds != *pending_fsm_wwds {
        return Err(storage_corruption_message(
            "OCOMP pending FSM membership does not exactly match the live scheduler",
        ));
    }
    Ok(unmatched_voting_windows)
}

fn voting_window(
    record: &outbe_ocomp_protocol::state::OcompJobRecordV1,
    deadline_height: u64,
    intent_id: alloy_primitives::B256,
) -> Result<Option<ResponseDeadlineKey>> {
    match record.status {
        OcompJobStatus::AwaitingFinality => {
            let expected = record
                .intent_height
                .checked_add(OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS)
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP awaiting-finality deadline overflow")
                })?;
            if deadline_height != expected {
                return Err(storage_corruption_message(
                    "OCOMP awaiting-finality FSM/job deadline mismatch",
                ));
            }
        }
        OcompJobStatus::VotingOpen => {
            let finalized = record.finalized.as_ref().ok_or_else(|| {
                storage_corruption_message("OCOMP voting FSM job is not finalized")
            })?;
            if finalized.deadline_height != deadline_height {
                return Err(storage_corruption_message(
                    "OCOMP voting FSM/job deadline mismatch",
                ));
            }
            return Ok(Some(ResponseDeadlineKey {
                deadline_height,
                job_id: finalized.job_id,
                intent_id,
            }));
        }
        _ => {
            return Err(storage_corruption_message(
                "terminal OCOMP job remains in the live scheduler",
            ))
        }
    }
    Ok(None)
}

fn validate_response_windows(
    contract: &MetadosisContract<'_>,
    mut unmatched_voting_windows: BTreeSet<ResponseDeadlineKey>,
    schema_limits: &SchemaLimits,
) -> Result<()> {
    for key in contract.read_response_deadline_index()? {
        let record = contract
            .ocomp_job_record(key.intent_id, schema_limits)?
            .ok_or_else(|| {
                storage_corruption_message("OCOMP response index points to a missing job")
            })?;
        if response_window_requires_live_fsm(&record, &key)?
            && !unmatched_voting_windows.remove(&key)
        {
            return Err(storage_corruption_message(
                "OCOMP response index has no matching live voting FSM",
            ));
        }
    }
    if !unmatched_voting_windows.is_empty() {
        return Err(storage_corruption_message(
            "OCOMP live voting FSM has no exact response deadline key",
        ));
    }
    Ok(())
}

fn response_window_requires_live_fsm(
    record: &outbe_ocomp_protocol::state::OcompJobRecordV1,
    key: &ResponseDeadlineKey,
) -> Result<bool> {
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| storage_corruption_message("OCOMP response index job is not finalized"))?;
    if finalized.job_id != key.job_id || finalized.deadline_height != key.deadline_height {
        return Err(storage_corruption_message(
            "OCOMP response index/job deadline mismatch",
        ));
    }
    match record.status {
        OcompJobStatus::VotingOpen => Ok(true),
        OcompJobStatus::Completed if finalized.quorum.is_some() => Ok(false),
        _ => Err(storage_corruption_message(
            "OCOMP response index points to a job without an open window",
        )),
    }
}

fn validate_ready_membership(
    contract: &MetadosisContract<'_>,
    active_set: &BTreeSet<WorldwideDay>,
    schema_limits: &SchemaLimits,
) -> Result<()> {
    for ready in contract.read_ready_index()? {
        if !active_set.contains(&ready.worldwide_day) {
            return Err(storage_corruption_message(
                "OCOMP READY index points outside active WWD index",
            ));
        }
        if contract
            .ocomp_fsm_states
            .get_bytes(&ready.worldwide_day)
            .is_empty()?
        {
            return Err(storage_corruption_message(
                "OCOMP READY index points to a missing FSM",
            ));
        }
        let state = contract.ocomp_fsm_state(ready.worldwide_day, schema_limits)?;
        let projection = state.projection();
        if projection.phase != DayPhase::Ready
            || projection.next_check_height != Some(ready.next_check_height)
            || projection.pending_nonce != ready.pending_nonce
        {
            return Err(storage_corruption_message(
                "OCOMP READY index does not match its FSM",
            ));
        }
    }
    Ok(())
}
