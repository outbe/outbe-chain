use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

use crate::{precompile::IMetadosis, schema::MetadosisContract};

use super::{
    request::fsm_limits,
    schema::{poc_schema_limits, OcompExpiryDisposition},
    state::{DayPhase, JobFsmProjection},
};

/// Runs the exact begin-zone expiry key. No WorldwideDay or job scan is
/// permitted on this path.
pub fn run_lifecycle_begin(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let schema_limits = poc_schema_limits();
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let Some(profile) = metadosis.read_ocomp_request_profile(&schema_limits)? else {
        return Ok(());
    };
    let limits = fsm_limits(&profile);
    let Some(state) = metadosis.live_ocomp_fsm_state(&schema_limits, limits)? else {
        return Ok(());
    };
    let before = state.projection();
    if before.phase != DayPhase::OffchainPending {
        return Ok(());
    }
    let deadline = before
        .deadline_height
        .ok_or_else(|| fatal("pending OCOMP state has no expiry height"))?;
    if ctx.block.block_number < deadline {
        return Ok(());
    }
    expire_exact(&mut metadosis, ctx, before, limits)
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
    let storage = ctx.storage.clone();
    storage.with_checkpoint(|| {
        let disposition = metadosis.expire_ocomp_job(
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
                    || metadosis.get_wwd_status(before.worldwide_day)?
                        != crate::schema::status::FAILED
                    || !metadosis.ocomp_scheduler.is_empty()?
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
    })
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
