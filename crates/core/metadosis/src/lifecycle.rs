//! EmissionDay and WorldwideDay lifecycle orchestration.

use outbe_emission::schema::metadosis_state;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, time::WorldwideDay};
use outbe_promislimit::PromisLimitContract;
use outbe_tributefactory::TributeFactoryContract;

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
};

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => MetadosisCallerError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    outbe_tributefactory::worldwideday::validate_wwd_timestamp(timestamp)
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
    {
        return Ok(None);
    }
    verify_record_wwd_backlink(&TributeFactoryContract::new(ctx.storage.clone()), &record)?;
    Ok(Some(record))
}

fn verify_record_wwd_backlink(
    factory: &TributeFactoryContract<'_>,
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

pub(crate) fn cancel_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    if metadosis.emission_days().get(day)?.is_some() {
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
    if metadosis.emission_days().get(day)?.is_some() {
        let (record, limit_minor) =
            outbe_emission::api::record_metadosis_failed(metadosis.storage.clone(), day)?;
        if !limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone()).checked_credit(limit_minor)?;
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
