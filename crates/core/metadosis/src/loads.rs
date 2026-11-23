//! Executes one Worldwide Day linked to a due EmissionDay: the Nods of the day already
//! hold their loads, and Metadosis splits its limit with their sum.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};
use outbe_system_common::metadosis_bindings::MetadosisStorageExt;
use outbe_system_common::time::WorldwideDay;

use crate::{
    constants::METADOSIS_FAILED_LOADS_ABOVE_LIMIT,
    lifecycle::{complete_metadosis, fail_metadosis},
    limits::{allocate_day_metadosis_limit, brief_auction, LysisLimitInput},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
};

/// The nominal and the Gratis loads of the Nods of one Worldwide Day.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DayLoads {
    pub(crate) nominal_minor: U256,
    pub(crate) loads_minor: U256,
}

/// Reads the sums of the Nods of `wwd` in the enclave. A day with no Nod has none.
pub(crate) fn day_loads(storage: &StorageHandle<'_>, wwd: WorldwideDay) -> Result<DayLoads> {
    if wwd.value() == 0 {
        return Ok(DayLoads::default());
    }
    let totals = outbe_worldwideday::day_totals(storage.clone(), wwd)?;
    if totals.nod_count == 0 {
        return Ok(DayLoads::default());
    }
    Ok(DayLoads {
        nominal_minor: outbe_nod::offers::read_day_sum(
            storage,
            outbe_nod::offers::day_nominal_scope(wwd),
            &totals.encrypted_nominal,
        )?,
        loads_minor: outbe_nod::offers::read_day_sum(
            storage,
            outbe_nod::offers::day_load_scope(wwd),
            &totals.encrypted_load,
        )?,
    })
}

fn record_completion(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    loads: &DayLoads,
    desis_limit: U256,
) -> Result<()> {
    let metadosis_limit = emission_day.metadosis_limit_minor();
    let promis_limit_added = metadosis_limit
        .checked_sub(loads.loads_minor)
        .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            lysis_limit: loads.loads_minor,
            desis_limit,
            nods: loads.loads_minor,
            promis_limit_added,
        },
    )?;
    complete_metadosis(metadosis, &updated)?;
    metadosis.emit(IMetadosis::MetadosisCompleted {
        emissionDay: emission_day.day,
        tributeNominalTotalMinor: loads.nominal_minor,
        gratisDemandMinor: loads.loads_minor,
        dayGratisLimitMinor: metadosis_limit,
        lysisLimitMinor: loads.loads_minor,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: loads.loads_minor,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    let loads = day_loads(&ctx.storage, emission_day.wwd)?;
    let base_limit = emission_day.metadosis_limit_minor();
    if loads.loads_minor > base_limit {
        return fail_metadosis(
            metadosis,
            emission_day.day,
            METADOSIS_FAILED_LOADS_ABOVE_LIMIT,
        );
    }
    let split = allocate_day_metadosis_limit(
        ctx.storage.clone(),
        LysisLimitInput {
            base_limit,
            lysis_limit_minor: loads.loads_minor,
            nominal_total: loads.nominal_minor,
            unused_lysis_minor: U256::ZERO,
        },
    )?;
    let desis_limit = brief_auction(ctx.storage.clone(), emission_day.day, &split)?;
    record_completion(metadosis, ctx, emission_day, &loads, desis_limit)
}
