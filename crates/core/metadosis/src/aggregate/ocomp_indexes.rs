use super::WwdProjection;
use crate::{
    errors::MetadosisStorageCorruptionError,
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
            return Err(
                MetadosisStorageCorruptionError::OcompStateExistsWithoutActiveProfile.into(),
            );
        }
        if !contract.read_ready_index()?.is_empty()
            || !contract.ocomp_scheduler.is_empty()?
            || !contract.read_response_deadline_index()?.is_empty()
        {
            return Err(
                MetadosisStorageCorruptionError::OcompStateExistsWithoutActiveProfile.into(),
            );
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
            return Err(
                MetadosisStorageCorruptionError::OcompLiveSchedulerPointsOutsideActiveWwdIndex
                    .into(),
            );
        }
        if !live_scheduler_wwds.insert(projection.worldwide_day) {
            return Err(
                MetadosisStorageCorruptionError::OcompLiveSchedulerContainsDuplicateWwd.into(),
            );
        }
        let deadline_height = projection
            .deadline_height
            .ok_or(MetadosisStorageCorruptionError::OcompLiveFsmMissingDeadline)?;
        let intent_id = projection
            .live_intent_id
            .ok_or(MetadosisStorageCorruptionError::OcompLiveFsmMissingLiveIntent)?;
        let record = contract
            .ocomp_job_record(intent_id, schema_limits)?
            .ok_or(MetadosisStorageCorruptionError::OcompLiveFsmMissingJobRecord)?;
        if let Some(key) = voting_window(&record, deadline_height, intent_id)? {
            unmatched_voting_windows.insert(key);
        }
    }
    if live_scheduler_wwds != *pending_fsm_wwds {
        return Err(
            MetadosisStorageCorruptionError::OcompPendingFsmMembershipMismatchLiveScheduler.into(),
        );
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
                .ok_or(MetadosisStorageCorruptionError::OcompAwaitingFinalityDeadlineOverflow)?;
            if deadline_height != expected {
                return Err(
                    MetadosisStorageCorruptionError::OcompAwaitingFinalityFsmJobDeadlineMismatch
                        .into(),
                );
            }
        }
        OcompJobStatus::VotingOpen => {
            let finalized = record
                .finalized
                .as_ref()
                .ok_or(MetadosisStorageCorruptionError::OcompVotingFsmJobNotFinalized)?;
            if finalized.deadline_height != deadline_height {
                return Err(
                    MetadosisStorageCorruptionError::OcompVotingFsmJobDeadlineMismatch.into(),
                );
            }
            return Ok(Some(ResponseDeadlineKey {
                deadline_height,
                job_id: finalized.job_id,
                intent_id,
            }));
        }
        _ => {
            return Err(
                MetadosisStorageCorruptionError::TerminalOcompJobRemainsInLiveScheduler.into(),
            )
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
            .ok_or(MetadosisStorageCorruptionError::OcompResponseIndexMissingJob)?;
        if response_window_requires_live_fsm(&record, &key)?
            && !unmatched_voting_windows.remove(&key)
        {
            return Err(
                MetadosisStorageCorruptionError::OcompResponseIndexMissingLiveVotingFsm.into(),
            );
        }
    }
    if !unmatched_voting_windows.is_empty() {
        return Err(
            MetadosisStorageCorruptionError::OcompLiveVotingFsmMissingExactResponseDeadlineKey
                .into(),
        );
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
        .ok_or(MetadosisStorageCorruptionError::OcompResponseIndexJobNotFinalized)?;
    if finalized.job_id != key.job_id || finalized.deadline_height != key.deadline_height {
        return Err(MetadosisStorageCorruptionError::OcompResponseIndexJobDeadlineMismatch.into());
    }
    match record.status {
        OcompJobStatus::VotingOpen => Ok(true),
        OcompJobStatus::Completed if finalized.quorum.is_some() => Ok(false),
        _ => Err(MetadosisStorageCorruptionError::OcompResponseIndexJobWithoutOpenWindow.into()),
    }
}

fn validate_ready_membership(
    contract: &MetadosisContract<'_>,
    active_set: &BTreeSet<WorldwideDay>,
    schema_limits: &SchemaLimits,
) -> Result<()> {
    for ready in contract.read_ready_index()? {
        if !active_set.contains(&ready.worldwide_day) {
            return Err(
                MetadosisStorageCorruptionError::OcompReadyIndexPointsOutsideActiveWwdIndex.into(),
            );
        }
        if contract
            .ocomp_fsm_states
            .get_bytes(&ready.worldwide_day)
            .is_empty()?
        {
            return Err(MetadosisStorageCorruptionError::OcompReadyIndexPointsToMissingFsm.into());
        }
        let state = contract.ocomp_fsm_state(ready.worldwide_day, schema_limits)?;
        let projection = state.projection();
        if projection.phase != DayPhase::Ready
            || projection.next_check_height != Some(ready.next_check_height)
            || projection.pending_nonce != ready.pending_nonce
        {
            return Err(MetadosisStorageCorruptionError::OcompReadyIndexMismatchFsm.into());
        }
    }
    Ok(())
}
