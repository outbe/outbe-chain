use alloy_primitives::B256;
use outbe_ocomp_protocol::state::OcompJobStatus;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

use crate::{precompile::IMetadosis, schema::MetadosisContract};

use super::{
    request::fsm_limits,
    schema::{poc_schema_limits, OcompExpiryDisposition},
    state::{DayPhase, JobFsmProjection},
    vote::ResponseWindowCloseV1,
};

/// Records finality for the exact live OCOMP request whose request block is the
/// consensus-certified parent. The lookup is bounded by the fork profile's
/// live-Job cap; unrelated finalized parents are a no-op.
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
    let limits = fsm_limits(&profile);
    let mut matched = None;
    for state in metadosis.live_ocomp_fsm_states(&schema_limits, limits)? {
        let intent_id = state
            .projection()
            .live_intent_id
            .ok_or_else(|| fatal("OCOMP live scheduler has no IntentId"))?;
        let record = metadosis
            .ocomp_job_record(intent_id, &schema_limits)?
            .ok_or_else(|| fatal("OCOMP live scheduler job is missing"))?;
        if record.intent_height == finalized_request_block_number
            && record.status == OcompJobStatus::AwaitingFinality
            && matched.replace(intent_id).is_some()
        {
            return Err(fatal(
                "multiple live OCOMP jobs claim the same request block",
            ));
        }
    }
    let Some(intent_id) = matched else {
        return Ok(false);
    };
    if finalized_request_state_root.is_zero() {
        return Err(fatal(
            "OCOMP certified request parent has no authenticated state root",
        ));
    }

    metadosis.record_ocomp_finality(
        intent_id,
        finalized_request_block_hash,
        finalized_request_state_root,
        ctx.block.block_number,
        outbe_ocomp_protocol::capacity::RESULT_DEADLINE_BLOCKS,
        &schema_limits,
    )?;
    Ok(true)
}

/// Runs the exact begin-zone expiry key. No WorldwideDay or job scan is
/// permitted on this path.
pub fn run_lifecycle_begin(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let schema_limits = poc_schema_limits();
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let Some(profile) = metadosis.read_ocomp_request_profile(&schema_limits)? else {
        return Ok(());
    };
    let limits = fsm_limits(&profile);

    // The OCOMP lifecycle must never stop block production. Its failures are
    // real and are logged at ERROR with the exact cause, but they are failures
    // of one WorldwideDay, not of the chain: the day is closed as `FAILED` and
    // whatever Lysis budget it still holds is credited back to the PromisLimit
    // carry-over, and the block continues without it.
    let storage = ctx.storage.clone();
    let outcome = storage
        .with_checkpoint(|| lifecycle_begin_exact(ctx, &mut metadosis, &schema_limits, limits));
    let Err(error) = outcome else {
        return Ok(());
    };
    tracing::error!(
        target: "outbe::ocomp",
        %error,
        block_number = ctx.block.block_number,
        "OCOMP lifecycle failed; failing the affected day instead of the block"
    );

    // Bounded by `max_pending_jobs`, so this is a fixed-cost probe, not a scan.
    for wwd in metadosis.live_ocomp_worldwide_days()? {
        if metadosis
            .ocomp_fsm_state(wwd, &schema_limits, limits)
            .is_ok()
        {
            continue;
        }
        let credited = metadosis.fail_ocomp_day_with_carry_over(wwd, &schema_limits)?;
        tracing::error!(
            target: "outbe::ocomp",
            wwd = wwd.value(),
            block_number = ctx.block.block_number,
            credited = %credited,
            "OCOMP day cannot advance; marked FAILED and its budget carried over"
        );
    }
    Ok(())
}

fn lifecycle_begin_exact(
    ctx: &BlockRuntimeContext<'_>,
    metadosis: &mut MetadosisContract<'_>,
    schema_limits: &outbe_ocomp_protocol::SchemaLimits,
    limits: super::state::JobFsmLimits,
) -> Result<()> {
    let schema_limits = *schema_limits;
    metadosis.open_due_ocomp_voting(ctx.block.block_number, &schema_limits, limits)?;
    let storage = ctx.storage.clone();
    storage.with_checkpoint(|| {
        match metadosis.close_due_ocomp_response_window(ctx.block.block_number, &schema_limits)? {
            ResponseWindowCloseV1::NotDue | ResponseWindowCloseV1::QuorumPreserved { .. } => Ok(()),
            ResponseWindowCloseV1::NoQuorum { intent_id } => {
                let state = metadosis
                    .live_ocomp_fsm_state_by_intent(intent_id, &schema_limits, limits)?
                    .ok_or_else(|| fatal("no-quorum OCOMP close has no live job"))?;
                let before = state.projection();
                if before.phase != DayPhase::OffchainPending
                    || before.live_intent_id != Some(intent_id)
                    || before
                        .deadline_height
                        .is_none_or(|deadline| deadline > ctx.block.block_number)
                {
                    return Err(fatal("no-quorum OCOMP close/live state mismatch"));
                }
                expire_exact(metadosis, ctx, before, limits)
            }
        }
    })
}

fn expire_exact(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    before: JobFsmProjection,
    limits: super::state::JobFsmLimits,
) -> Result<()> {
    let intent_id = before
        .live_intent_id
        .ok_or_else(|| fatal("pending OCOMP state has no live IntentId"))?;
    let old_pending_nonce = before.pending_nonce;
    let disposition = metadosis.expire_ocomp_job(
        intent_id,
        ctx.block.block_number,
        ctx.block.timestamp,
        &poc_schema_limits(),
        limits,
    )?;
    let expected_next = old_pending_nonce
        .checked_add(1)
        .ok_or_else(|| fatal("OCOMP pending nonce overflow after expiry"))?;
    match disposition {
        OcompExpiryDisposition::RetryScheduled { next_pending_nonce } => {
            let after = metadosis
                .ocomp_fsm_state(before.worldwide_day, &poc_schema_limits(), limits)?
                .projection();
            if next_pending_nonce != expected_next
                || after.phase != DayPhase::Ready
                || after.pending_nonce != expected_next
                || after.next_check_height != ctx.block.block_number.checked_add(1)
                || after.live_intent_id.is_some()
            {
                return Err(fatal("OCOMP expiry retry post-state is inconsistent"));
            }
        }
        OcompExpiryDisposition::TerminalNoRetry {
            next_pending_nonce,
            carry_over,
        } => {
            let expected_budget = before
                .retained_lysis_budget
                .ok_or_else(|| fatal("terminal OCOMP expiry has no retained budget"))?;
            if next_pending_nonce != expected_next
                || carry_over.credited != expected_budget
                || carry_over.after
                    != carry_over
                        .before
                        .checked_add(expected_budget)
                        .ok_or_else(|| fatal("terminal OCOMP carry-over proof overflow"))?
                || metadosis.get_wwd_status(before.worldwide_day)? != crate::schema::status::FAILED
                || metadosis
                    .live_ocomp_fsm_state_by_intent(intent_id, &poc_schema_limits(), limits)?
                    .is_some()
                || !metadosis
                    .ocomp_fsm_states
                    .get_bytes(&before.worldwide_day)
                    .is_empty()?
            {
                return Err(fatal("OCOMP terminal no-retry post-state is inconsistent"));
            }
        }
    }
    metadosis.emit(IMetadosis::OffchainJobExpired {
        intentId: intent_id,
        wwd: before.worldwide_day.value(),
        oldPendingNonce: old_pending_nonce,
        nextPendingNonce: expected_next,
        expiredAtHeight: ctx.block.block_number,
    })
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
