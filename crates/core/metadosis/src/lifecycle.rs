//! EmissionDay and WorldwideDay lifecycle orchestration.

use alloy_primitives::U256;
use outbe_emission::{api::get_emission_day, schema::metadosis_state, EmissionDay};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, Result},
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::time::WorldwideDay;
use outbe_worldwideday::WorldwideDayContract;

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::MetadosisContract,
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
    let Some(record) = get_emission_day(metadosis.storage.clone(), day)? else {
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

pub(crate) fn cancel_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    cancel_auction_of(metadosis, day)?;
    if get_emission_day(metadosis.storage.clone(), day)?.is_some() {
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
    if let Some(pending) = get_emission_day(metadosis.storage.clone(), day)? {
        let loads_minor = match crate::loads::day_loads(&metadosis.storage, pending.wwd) {
            Ok(loads) => loads.loads_minor,
            // An unreadable day sum leaves the whole limit with the Nods: nothing goes back.
            Err(err) if err.record_pass_failure() == PassFailure::Skip => U256::MAX,
            Err(err) => return Err(err),
        };
        let (record, limit_minor) = outbe_emission::api::record_metadosis_failed(
            metadosis.storage.clone(),
            day,
            loads_minor,
        )?;
        if !limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone())
                .add(outbe_promislimit::LimitSource::MetadosisLimit, limit_minor)?;
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
