use alloy_primitives::B256;
use outbe_ocomp_protocol::state::OcompJobStatus;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::{
    aggregate::ValidatedWwdAggregate,
    errors::MetadosisStorageCorruptionError,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition},
    schema::MetadosisContract,
};

use super::{
    schema::poc_schema_limits,
    state::{DayPhase, JobFsmProjection},
    vote::{OcompPenaltyMetrics, ResponseWindowCloseV1},
    FinalityAnchor,
};

/// Records finality for the exact live OCOMP request whose request block is the
/// consensus-certified parent. The canonical retained WorldwideDay population
/// bounds the lookup. Unrelated finalized parents are a no-op.
pub fn record_certified_parent_finality(
    ctx: &BlockRuntimeContext<'_>,
    finalized_request_block_number: u64,
    finalized_request_block_hash: B256,
    finalized_request_state_root: B256,
) -> Result<bool> {
    let schema_limits = poc_schema_limits();
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let Some(profile) = metadosis.read_ocomp_request_profile(&schema_limits)? else {
        return Ok(false);
    };
    let mut matched = None;
    for state in metadosis.live_ocomp_fsm_states(&schema_limits)? {
        let intent_id = state
            .projection()
            .live_intent_id
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerHasNoIntentId)?;
        let record = metadosis
            .ocomp_job_record(intent_id, &schema_limits)?
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerJobMissing)?;
        if record.intent_height == finalized_request_block_number
            && record.status == OcompJobStatus::AwaitingFinality
            && matched.replace(intent_id).is_some()
        {
            return Err(
                MetadosisStorageCorruptionError::MultipleLiveOcompJobsClaimSameRequestBlock.into(),
            );
        }
    }
    let Some(intent_id) = matched else {
        return Ok(false);
    };
    if finalized_request_state_root.is_zero() {
        return Err(
            MetadosisStorageCorruptionError::OcompCertifiedRequestParentMissingStateRoot.into(),
        );
    }

    metadosis.record_ocomp_finality(
        intent_id,
        FinalityAnchor {
            request_block_hash: finalized_request_block_hash,
            request_state_root: finalized_request_state_root,
            recorded_height: ctx.block.block_number,
            response_window_blocks: profile.capacity_profile.result_deadline_blocks,
        },
        &schema_limits,
    )?;
    Ok(true)
}

/// Process begin-zone expiry, including missed lifecycle boundaries.
/// This path scans live jobs and validates the bounded WorldwideDay aggregate.
pub fn run_lifecycle_begin_with_scope(
    ctx: &BlockRuntimeContext<'_>,
) -> Result<OcompPenaltyMetrics> {
    if let Some(before) = missed_lifecycle_boundary(ctx)? {
        let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
        let current = aggregate
            .record(before.worldwide_day)
            .ok_or(MetadosisStorageCorruptionError::OverdueOcompExpiryMissingPersistedOuterWwd)?;
        let outer_transition = reduce_outer_wwd(Some(current), OuterWwdEvent::OcompExpired)?;
        expire_exact(
            &mut MetadosisContract::new(ctx.storage.clone()),
            ctx,
            before,
            &outer_transition,
        )?;
        return Ok(OcompPenaltyMetrics::default());
    }
    run_lifecycle_begin_exact(ctx)
}

fn run_lifecycle_begin_exact(ctx: &BlockRuntimeContext<'_>) -> Result<OcompPenaltyMetrics> {
    let schema_limits = poc_schema_limits();
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let Some(_profile) = metadosis.read_ocomp_request_profile(&schema_limits)? else {
        return Ok(OcompPenaltyMetrics::default());
    };
    metadosis.open_due_ocomp_voting(ctx.block.block_number, &schema_limits)?;
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    let response =
        metadosis.close_due_ocomp_response_window(ctx.block.block_number, &schema_limits)?;
    let metrics = response.metrics;
    close_response_attempt(&mut metadosis, ctx, &aggregate, response.close)?;
    let Some(before) = due_unfinalized_attempt(&metadosis, ctx.block.block_number)? else {
        return Ok(metrics);
    };
    let current = aggregate
        .record(before.worldwide_day)
        .ok_or(MetadosisStorageCorruptionError::AwaitingFinalityExpiryMissingPersistedOuterWwd)?;
    let outer_transition = reduce_outer_wwd(Some(current), OuterWwdEvent::OcompExpired)?;
    expire_exact(&mut metadosis, ctx, before, &outer_transition)?;
    Ok(metrics)
}

fn close_response_attempt(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    aggregate: &ValidatedWwdAggregate,
    close: ResponseWindowCloseV1,
) -> Result<()> {
    let schema_limits = poc_schema_limits();
    match close {
        ResponseWindowCloseV1::NotDue | ResponseWindowCloseV1::QuorumPreserved { .. } => Ok(()),
        ResponseWindowCloseV1::NoQuorum { intent_id } => {
            let state = metadosis
                .live_ocomp_fsm_state_by_intent(intent_id, &schema_limits)?
                .ok_or(MetadosisStorageCorruptionError::NoQuorumOcompCloseMissingLiveJob)?;
            let before = state.projection();
            if before.phase != DayPhase::OffchainPending
                || before.live_intent_id != Some(intent_id)
                || before
                    .deadline_height
                    .is_none_or(|deadline| deadline > ctx.block.block_number)
            {
                return Err(
                    MetadosisStorageCorruptionError::NoQuorumOcompCloseLiveStateMismatch.into(),
                );
            }
            let current = aggregate.record(before.worldwide_day).ok_or(
                MetadosisStorageCorruptionError::NoQuorumOcompCloseMissingPersistedOuterWwd,
            )?;
            let outer_transition = reduce_outer_wwd(Some(current), OuterWwdEvent::OcompExpired)?;
            expire_exact(metadosis, ctx, before, &outer_transition)
        }
    }
}

fn due_unfinalized_attempt(
    metadosis: &MetadosisContract<'_>,
    at_height: u64,
) -> Result<Option<JobFsmProjection>> {
    let schema_limits = poc_schema_limits();
    let mut due = None;
    for state in metadosis.live_ocomp_fsm_states(&schema_limits)? {
        let before = state.projection();
        let intent_id = before
            .live_intent_id
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerHasNoIntentId)?;
        let record = metadosis
            .ocomp_job_record(intent_id, &schema_limits)?
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerJobMissing)?;
        if record.status != OcompJobStatus::AwaitingFinality || record.finalized.is_some() {
            continue;
        }
        let deadline = before
            .deadline_height
            .ok_or(MetadosisStorageCorruptionError::OcompAwaitingFinalityJobMissingDeadline)?;
        if deadline > at_height {
            continue;
        }
        if deadline < at_height {
            return Err(
                MetadosisStorageCorruptionError::OcompConsensusSkippedAwaitingFinalityExpiryHeight
                    .into(),
            );
        }
        if due.replace(before).is_some() {
            return Err(
                MetadosisStorageCorruptionError::MultipleOcompAwaitingFinalityJobsDueAtOneHeight
                    .into(),
            );
        }
    }
    Ok(due)
}

fn missed_lifecycle_boundary(ctx: &BlockRuntimeContext<'_>) -> Result<Option<JobFsmProjection>> {
    let schema_limits = poc_schema_limits();
    let metadosis = MetadosisContract::new(ctx.storage.clone());
    let Some(_profile) = metadosis.read_ocomp_request_profile(&schema_limits)? else {
        return Ok(None);
    };
    for state in metadosis.live_ocomp_fsm_states(&schema_limits)? {
        let projection = state.projection();
        let intent_id = projection
            .live_intent_id
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerHasNoIntentId)?;
        let record = metadosis
            .ocomp_job_record(intent_id, &schema_limits)?
            .ok_or(MetadosisStorageCorruptionError::OcompLiveSchedulerJobMissing)?;
        if record.status != OcompJobStatus::AwaitingFinality {
            continue;
        }
        let missed_boundary = match record.finalized.as_ref() {
            Some(finalized) => ctx.block.block_number >= finalized.deadline_height,
            None => projection
                .deadline_height
                .is_some_and(|deadline| ctx.block.block_number > deadline),
        };
        if missed_boundary {
            return Ok(Some(projection));
        }
    }
    Ok(None)
}

fn expire_exact(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    before: JobFsmProjection,
    outer_transition: &OuterWwdTransition,
) -> Result<()> {
    let intent_id = before
        .live_intent_id
        .ok_or(MetadosisStorageCorruptionError::PendingOcompStateMissingLiveIntentId)?;
    let retained_lysis_limit_minor = metadosis.expire_ocomp_job(
        outer_transition,
        intent_id,
        ctx.block.block_number,
        ctx.block.timestamp,
        &poc_schema_limits(),
    )?;
    let expected_retained_limit_minor = before
        .retained_lysis_limit_minor
        .ok_or(MetadosisStorageCorruptionError::TerminalOcompExpiryMissingRetainedLimit)?;
    if retained_lysis_limit_minor != expected_retained_limit_minor {
        return Err(
            MetadosisStorageCorruptionError::TerminalOcompExpiryRetainedLimitMismatch.into(),
        );
    }
    let value_routed = metadosis
        .request_limit_receipt(before.worldwide_day, &poc_schema_limits())?
        .ok_or_else(|| {
            outbe_primitives::error::PrecompileError::from(
                MetadosisStorageCorruptionError::ExpiredOcompDayMissingRequestReceipt,
            )
        })
        .and_then(|receipt| crate::ocomp_limits::retained_request_limit(&receipt))?;
    crate::terminal::fail_expired_ocomp_day(
        ctx.storage.clone(),
        crate::terminal::ExpiredFailure {
            settlement: crate::terminal::FailureSettlement {
                block_number: ctx.block.block_number,
                worldwide_day: before.worldwide_day,
                unused_limit: value_routed,
            },
            intent_id,
            outer_transition,
        },
    )?;
    validate_expired_post_state(metadosis, before, intent_id, value_routed)?;
    metadosis.emit(IMetadosis::OffchainJobExpired {
        intentId: intent_id,
        wwd: before.worldwide_day.value(),
        expiredAtHeight: ctx.block.block_number,
    })
}

fn validate_expired_post_state(
    metadosis: &MetadosisContract<'_>,
    before: JobFsmProjection,
    intent_id: B256,
    value_routed: alloy_primitives::U256,
) -> Result<()> {
    let inconsistent = || -> outbe_primitives::error::PrecompileError {
        MetadosisStorageCorruptionError::OcompExpiryPostStateInconsistent.into()
    };
    if metadosis.get_wwd_status(before.worldwide_day)? != crate::aggregate::WwdStatus::Failed {
        return Err(inconsistent());
    }
    if metadosis
        .read_metadosis_failure_receipt(before.worldwide_day, value_routed)?
        .is_none()
    {
        return Err(inconsistent());
    }
    if metadosis
        .live_ocomp_fsm_state_by_intent(intent_id, &poc_schema_limits())?
        .is_some()
    {
        return Err(inconsistent());
    }
    if !metadosis
        .ocomp_fsm_states
        .get_bytes(&before.worldwide_day)
        .is_empty()?
    {
        return Err(inconsistent());
    }
    Ok(())
}
