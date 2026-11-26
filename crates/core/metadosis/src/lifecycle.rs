//! EmissionDay and WorldwideDay lifecycle orchestration.

use alloy_primitives::U256;
use outbe_emission::schema::metadosis_state;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, Result},
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::time::WorldwideDay;
use outbe_worldwideday::WorldwideDayContract;

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    limits::LoadsCoverage,
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
};

pub(crate) fn date_step_error(
    err: outbe_system_common::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => MetadosisCallerError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    outbe_worldwideday::validate_wwd_timestamp(timestamp)
}

/// The record of `day` when its Metadosis is due: the allocation gave it its limit, and
/// Metadosis has not run.
pub(crate) fn ready_emission_day(
    metadosis: &MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
) -> Result<Option<EmissionDay>> {
    let Some(record) = metadosis.emission_days().get(day)? else {
        return Ok(None);
    };
    if record.state != outbe_emission::schema::emission_day_state::PROCESSING
        || record.metadosis_state != metadosis_state::PENDING
        || ctx.block.timestamp < record.metadosis_start
    {
        return Ok(None);
    }
    verify_record_wwd_backlink(&WorldwideDayContract::new(ctx.storage.clone()), &record)?;
    Ok(Some(record))
}

fn verify_record_wwd_backlink(
    factory: &WorldwideDayContract<'_>,
    record: &EmissionDay,
) -> Result<()> {
    if record.wwd.value() == 0 {
        return Ok(());
    }
    let Some(wwd_record) = factory.worldwide_days.get(record.wwd)? else {
        return Ok(());
    };
    if wwd_record.emission_day != record.day {
        return Err(
            MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                day: record.day,
                expected_wwd: record.wwd,
                found_wwd: WorldwideDay::new(wwd_record.emission_day),
            }
            .into(),
        );
    }
    Ok(())
}

pub(crate) fn complete_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    if !outbe_agentreward::distribution::has_pending_accruals_for_day(
        &metadosis.storage,
        emission_day.day,
    )? {
        outbe_emission::api::complete_day_emission_limit(
            metadosis.storage.clone(),
            emission_day.day,
        )?;
    }
    Ok(())
}

/// Ask Desis to cancel the auction of a day whose Metadosis did not complete. A Desis revert does
/// not stop the failure path: an auction with no brief cancels itself at the end of its reveal stage.
fn cancel_auction_of(metadosis: &MetadosisContract<'_>, day: u32) -> Result<()> {
    let storage = metadosis.storage.clone();
    match storage
        .clone()
        .with_checkpoint(|| outbe_desis::api::cancel_auction(storage.clone(), day.into()))
    {
        Err(err) if err.record_pass_failure() == PassFailure::Skip => {
            tracing::warn!(target: "outbe::metadosis", day, error = ?err, "auction cancel skipped");
            Ok(())
        }
        result => result,
    }
}

/// How the limit of the day covers its loads and `excess`, or `None` when the day sum cannot be read.
fn cover_loads(
    metadosis: &MetadosisContract<'_>,
    pending: &EmissionDay,
    excess: U256,
) -> Result<Option<LoadsCoverage>> {
    match crate::loads::day_loads(&metadosis.storage, pending.wwd) {
        Ok(loads) => Ok(Some(LoadsCoverage::of(
            pending.metadosis_limit_minor(),
            loads.loads_minor,
            excess,
        )?)),
        // Only corrupt sealed data fails the read; the whole limit stays with the Nods.
        Err(err) if err.record_pass_failure() == PassFailure::Skip => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn cancel_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    cancel_auction_of(metadosis, day)?;
    if let Some(pending) = metadosis.emission_days().get(day)? {
        let excess = outbe_emission::api::excess_nod_loads(metadosis.storage.clone())?;
        if let Some(coverage) = cover_loads(metadosis, &pending, excess)? {
            crate::loads::carry_excess(metadosis, day, excess, &coverage)?;
        }
        let record =
            outbe_emission::api::record_metadosis_canceled(metadosis.storage.clone(), day)?;
        if !outbe_agentreward::distribution::has_pending_accruals_for_day(
            &metadosis.storage,
            record.day,
        )? {
            outbe_emission::api::complete_day_emission_limit(
                metadosis.storage.clone(),
                record.day,
            )?;
        }
    }
    metadosis.emit(IMetadosis::MetadosisCanceled {
        emissionDay: day,
        reason,
    })
}

pub(crate) fn fail_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    cancel_auction_of(metadosis, day)?;
    if let Some(pending) = metadosis.emission_days().get(day)? {
        let excess = outbe_emission::api::excess_nod_loads(metadosis.storage.clone())?;
        let coverage = cover_loads(metadosis, &pending, excess)?;
        let (record, limit_minor) = outbe_emission::api::record_metadosis_failed(
            metadosis.storage.clone(),
            day,
            coverage.map_or(U256::MAX, |coverage| coverage.covered_minor),
        )?;
        if !limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone())
                .add(outbe_promislimit::LimitSource::MetadosisLimit, limit_minor)?;
        }
        if let Some(coverage) = coverage {
            crate::loads::carry_excess(metadosis, day, excess, &coverage)?;
        }
        if !outbe_agentreward::distribution::has_pending_accruals_for_day(
            &metadosis.storage,
            record.day,
        )? {
            outbe_emission::api::complete_day_emission_limit(
                metadosis.storage.clone(),
                record.day,
            )?;
        }
    }
    metadosis.emit(IMetadosis::MetadosisFailed {
        emissionDay: day,
        reason,
    })
}
