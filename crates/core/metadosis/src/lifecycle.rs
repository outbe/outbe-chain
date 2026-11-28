//! EmissionDay and WorldwideDay lifecycle orchestration.

use alloy_primitives::U256;
use outbe_emission::{api::get_emission_day, schema::metadosis_state, EmissionDay};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, Result},
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::time::WorldwideDay;

use crate::{
    errors::MetadosisStorageCorruptionError, limits::LoadsCoverage, precompile::IMetadosis,
    schema::MetadosisContract,
};

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    outbe_worldwideday::validate_wwd_timestamp(timestamp)
}

/// The record of `day` when its Metadosis is due: the day started, its offering ended, and
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
    verify_record_wwd_backlink(ctx, &record)?;
    Ok(Some(record))
}

fn verify_record_wwd_backlink(ctx: &BlockRuntimeContext<'_>, record: &EmissionDay) -> Result<()> {
    if record.wwd.value() == 0 {
        return Ok(());
    }
    let Some(wwd_record) = outbe_worldwideday::get_worldwide_day(ctx.storage.clone(), record.wwd)?
    else {
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

/// Ask Desis to cancel the auction of a day whose Metadosis failed. A Desis revert does not stop
/// the failure path: an auction with no Desis limit cancels itself at the end of its reveal stage.
fn cancel_desis_of(metadosis: &MetadosisContract<'_>, day: u32) -> Result<()> {
    let storage = metadosis.storage.clone();
    match storage
        .clone()
        .with_checkpoint(|| outbe_desis::api::cancel_desis(storage.clone(), day.into()))
    {
        Err(err) if err.record_pass_failure() == PassFailure::Skip => {
            tracing::warn!(target: "outbe::metadosis", day, error = ?err, "Desis cancel skipped");
            Ok(())
        }
        result => result,
    }
}

/// The loads of the day and how its limit covers them and `excess`, or `None` when the day sums
/// cannot be read.
fn cover_loads(
    metadosis: &MetadosisContract<'_>,
    pending: &EmissionDay,
    excess: U256,
) -> Result<Option<(crate::loads::DayLoads, LoadsCoverage)>> {
    match crate::loads::day_loads(&metadosis.storage, pending.wwd) {
        Ok(loads) => Ok(Some((
            loads,
            LoadsCoverage::of(pending.metadosis_limit_minor(), loads.loads_minor, excess)?,
        ))),
        // Only corrupt sealed data fails the read; the whole limit stays with the Nods.
        Err(err) if err.record_pass_failure() == PassFailure::Skip => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn fail_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    cancel_desis_of(metadosis, day)?;
    if let Some(pending) = get_emission_day(metadosis.storage.clone(), day)? {
        let excess = outbe_emission::api::excess_nod_loads(metadosis.storage.clone())?;
        let covered = cover_loads(metadosis, &pending, excess)?;
        let limit = pending.metadosis_limit_minor();
        let amounts = match &covered {
            Some((loads, coverage)) => loads.amounts(limit, coverage.covered_minor, U256::ZERO),
            None => outbe_emission::api::MetadosisAmounts {
                nods: U256::MAX,
                ..Default::default()
            },
        };
        let (_, rest) =
            outbe_emission::api::record_metadosis_failed(metadosis.storage.clone(), day, amounts)?;
        if !rest.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone())
                .add(outbe_promislimit::LimitSource::MetadosisLimit, rest)?;
        }
        if let Some((_, coverage)) = covered {
            crate::loads::carry_excess(metadosis, day, excess, &coverage)?;
        }
    }
    metadosis.emit(IMetadosis::MetadosisFailed {
        emissionDay: day,
        reason,
    })
}
