//! Executes one Worldwide Day linked to a due EmissionDay: the Nods of the day already
//! hold their loads, and Metadosis splits its limit with their sum.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};
use outbe_system_common::metadosis_bindings::MetadosisStorageExt;
use outbe_system_common::time::WorldwideDay;

use crate::{
    lifecycle::complete_metadosis,
    limits::{allocate_day_metadosis_limit, apply_desis_limit, LimitSplit, LoadsCoverage},
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
    let totals = outbe_worldwideday::worldwide_day_totals(storage.clone(), wwd)?;
    if totals.nod_count == 0 {
        return Ok(DayLoads::default());
    }
    Ok(DayLoads {
        nominal_minor: outbe_nod::offers::read_worldwide_day_sum(
            storage,
            outbe_nod::offers::worldwide_day_nominal_scope(wwd),
            &totals.encrypted_nominal,
        )?,
        loads_minor: outbe_nod::offers::read_worldwide_day_sum(
            storage,
            outbe_nod::offers::worldwide_day_load_scope(wwd),
            &totals.encrypted_load,
        )?,
    })
}

/// Records the excess the day leaves, and announces it while some loads stay uncovered.
pub(crate) fn carry_excess(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    before: U256,
    coverage: &LoadsCoverage,
) -> Result<()> {
    if coverage.excess_minor != before {
        outbe_emission::api::record_excess_nod_loads(
            metadosis.storage.clone(),
            coverage.excess_minor,
        )?;
    }
    if coverage.excess_minor.is_zero() {
        return Ok(());
    }
    metadosis.emit(IMetadosis::MetadosisLimitExceeded {
        emissionDay: day,
        excessMinor: coverage.excess_minor,
    })
}

fn record_completion(
    metadosis: &mut MetadosisContract<'_>,
    emission_day: &EmissionDay,
    loads: &DayLoads,
    coverage: &LoadsCoverage,
    desis_limit: U256,
) -> Result<()> {
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            desis_limit,
            nods: coverage.covered_minor,
            promis_limit_added: coverage.promis_limit_add,
        },
    )?;
    complete_metadosis(metadosis, &updated)?;
    metadosis.emit(IMetadosis::MetadosisCompleted {
        emissionDay: emission_day.day,
        nominalAmountMinor: loads.nominal_minor,
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
    let excess = outbe_emission::api::excess_nod_loads(ctx.storage.clone())?;
    let coverage = LoadsCoverage::of(base_limit, loads.loads_minor, excess)?;
    let split = if coverage.excess_minor.is_zero() {
        allocate_day_metadosis_limit(
            ctx.storage.clone(),
            base_limit - excess,
            loads.loads_minor,
            loads.nominal_minor,
        )?
    } else {
        LimitSplit::uncovered(base_limit)
    };
    let desis_limit = apply_desis_limit(ctx.storage.clone(), emission_day.day, &split)?;
    carry_excess(metadosis, emission_day.day, excess, &coverage)?;
    record_completion(metadosis, emission_day, &loads, &coverage, desis_limit)
}
