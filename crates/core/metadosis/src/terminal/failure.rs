use super::MetadosisFailureReceipt;
use crate::{
    aggregate::{ValidatedWwdAggregate, WwdStatus},
    commit::commit_outer_transition,
    ocomp::schema::poc_schema_limits,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind},
    schema::MetadosisContract,
};
use alloy_primitives::{B256, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

/// Atomically closes one deterministic Metadosis protocol failure without
/// discarding its immutable OCOMP evidence.
pub(crate) fn fail_worldwide_day(
    storage: StorageHandle<'_>,
    block_number: u64,
    scope: &ExecutionScope,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(storage.clone())?;
    let current = aggregate.record(worldwide_day).ok_or_else(|| {
        crate::errors::storage_corruption("Metadosis failure has no persisted WorldwideDay".into())
    })?;
    let mut metadosis = MetadosisContract::new(storage.clone());
    let limits = poc_schema_limits();
    let unused_limit = metadosis
        .request_limit_receipt(worldwide_day, &limits)?
        .map_or(current.metadosis_limit_amount, |receipt| {
            receipt.lysis_limit_minor
        });

    if current.status == WwdStatus::Failed {
        metadosis
            .read_metadosis_failure_receipt(worldwide_day, unused_limit)?
            .ok_or_else(|| {
                crate::errors::storage_corruption(
                    "failed Metadosis WWD has no failure receipt".into(),
                )
            })?;
        return Ok(());
    }
    if current.status.is_terminal() {
        return Err(crate::errors::storage_corruption(
            "Metadosis failure cannot replace a completed WWD".into(),
        ));
    }

    let _profile = metadosis
        .read_ocomp_request_profile(&limits)?
        .ok_or_else(|| {
            crate::errors::storage_corruption("Metadosis failure has no OCOMP profile".into())
        })?;
    metadosis.clear_ready_ocomp_for_failed_day(worldwide_day, &limits)?;
    let credit = PromisLimitContract::new(storage.clone()).checked_add_carry_over(unused_limit)?;
    // Retirement is deliberately the final compressed-entity mutation. All
    // other failure effects are prepared first and the enclosing checkpoint
    // rolls them back together if retirement or the final block seal fails.
    let tribute = TributeContract::new(storage).forfeit_sealed_partition(scope, worldwide_day)?;
    metadosis.write_metadosis_failure_receipt(MetadosisFailureReceipt {
        worldwide_day,
        value_routed: credit.credited,
        carry_over_before: credit.before,
        carry_over_after: credit.after,
        retirement: tribute.retirement_outcome,
        block_number,
    })?;
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::EmergencyFail)?;
    commit_outer_transition(&mut metadosis, worldwide_day, &transition, block_number)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: worldwide_day.into(),
        tributeTotals: tribute.forfeited_nominal,
        dayGratisDemand: U256::ZERO,
        dayGratisLimit: U256::ZERO,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        dayMetadosisLimitRemainder: unused_limit,
        status: "FAILED".into(),
        blockNumber: block_number,
    })
}

/// Completes an OCOMP expiry as the same atomic FAILED contract
/// used by every other exact-WWD business failure. The expiry transition has
/// already written immutable `Expired` attempt evidence, but the live FSM and
/// outer WWD remain active until this function credits the retained Lysis Limit,
/// retires Tribute as the final CE mutation, and commits the terminal state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fail_expired_ocomp_day(
    storage: StorageHandle<'_>,
    block_number: u64,
    scope: &ExecutionScope,
    worldwide_day: WorldwideDay,
    intent_id: B256,
    unused_limit: U256,
    outer_transition: &OuterWwdTransition,
) -> Result<()> {
    if !matches!(
        outer_transition.kind(),
        OuterWwdTransitionKind::OcompExpired
    ) {
        return Err(crate::errors::storage_corruption(
            "expired OCOMP failure requires the typed expiry transition".into(),
        ));
    }

    let limits = poc_schema_limits();
    let mut metadosis = MetadosisContract::new(storage.clone());
    let record = metadosis
        .ocomp_job_record(intent_id, &limits)?
        .ok_or_else(|| crate::errors::storage_corruption("expired OCOMP job is missing".into()))?;
    if !expired_job_binding(&record, worldwide_day, unused_limit) {
        return Err(expired_prestate_error());
    }
    if metadosis.terminal_intent_count(worldwide_day)? != 1
        || metadosis
            .ocomp_fsm_states
            .get_bytes(&worldwide_day)
            .is_empty()?
        || metadosis.get_wwd_status(worldwide_day)? != WwdStatus::OffchainPending
    {
        return Err(expired_prestate_error());
    }
    if !expired_evidence(&record) {
        return Err(expired_prestate_error());
    }

    let credit = PromisLimitContract::new(storage.clone()).checked_add_carry_over(unused_limit)?;
    // Retirement is deliberately the final compressed-entity mutation.
    let tribute = TributeContract::new(storage).forfeit_sealed_partition(scope, worldwide_day)?;
    metadosis.write_metadosis_failure_receipt(MetadosisFailureReceipt {
        worldwide_day,
        value_routed: credit.credited,
        carry_over_before: credit.before,
        carry_over_after: credit.after,
        retirement: tribute.retirement_outcome,
        block_number,
    })?;
    commit_outer_transition(
        &mut metadosis,
        worldwide_day,
        outer_transition,
        block_number,
    )?;
    metadosis
        .ocomp_fsm_states
        .get_bytes(&worldwide_day)
        .clear()?;
    metadosis.remove_live_scheduler(intent_id)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: worldwide_day.into(),
        tributeTotals: tribute.forfeited_nominal,
        dayGratisDemand: U256::ZERO,
        dayGratisLimit: U256::ZERO,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        dayMetadosisLimitRemainder: unused_limit,
        status: "FAILED".into(),
        blockNumber: block_number,
    })
}

fn expired_prestate_error() -> outbe_primitives::error::PrecompileError {
    crate::errors::storage_corruption("expired OCOMP failure pre-state is inconsistent".into())
}
fn expired_job_binding(
    record: &outbe_ocomp_protocol::state::OcompJobRecordV1,
    worldwide_day: WorldwideDay,
    unused_limit: U256,
) -> bool {
    record.intent.wwd == worldwide_day.value()
        && record.intent.frozen_metadosis_values.lysis_limit_minor == unused_limit
}
fn expired_evidence(record: &outbe_ocomp_protocol::state::OcompJobRecordV1) -> bool {
    record.status == outbe_ocomp_protocol::state::OcompJobStatus::Expired
        && record.terminal.as_ref().is_some_and(|terminal| {
            terminal.outcome == outbe_ocomp_protocol::state::OcompTerminalOutcome::Expired
                && terminal.completed_binding.is_none()
        })
}
