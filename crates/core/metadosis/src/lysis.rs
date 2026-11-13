//! Executes one sealed WorldwideDay linked to a due EmissionDay.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_tribute::{PricedCurrencies, TributeContract};

use crate::{
    lifecycle::complete_metadosis,
    limits::{allocate_day_metadosis_limit, brief_auction, LysisLimitInput},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
};

#[derive(Clone, Copy, Debug, Default)]
struct LysisTotals {
    /// Nominal of the priced Tributes.
    nominal_minor: U256,
    gratis_demand_minor: U256,
    lysis_limit_minor: U256,
    /// What the Nods of the day take of the Lysis Limit.
    used_minor: U256,
    unused_minor: U256,
}

impl LysisTotals {
    fn new(outcome: outbe_tribute::LysisOutcome) -> Self {
        let used_minor = outcome.allocated_minor.min(outcome.lysis_limit_minor);
        Self {
            nominal_minor: outcome.nominal_minor,
            gratis_demand_minor: outcome.gratis_demand_minor,
            lysis_limit_minor: outcome.lysis_limit_minor,
            used_minor,
            unused_minor: outcome.lysis_limit_minor - used_minor,
        }
    }
}

fn record_completion(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    totals: &LysisTotals,
    desis_limit: U256,
) -> Result<()> {
    let metadosis_limit = emission_day.metadosis_limit_minor();
    let promis_limit_added = metadosis_limit
        .checked_sub(totals.used_minor)
        .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            lysis_limit: totals.lysis_limit_minor,
            desis_limit,
            nods: totals.used_minor,
            promis_limit_added,
        },
    )?;
    complete_metadosis(metadosis, &updated)?;
    metadosis.emit(IMetadosis::MetadosisCompleted {
        emissionDay: emission_day.day,
        tributeNominalTotalMinor: totals.nominal_minor,
        gratisDemandMinor: totals.gratis_demand_minor,
        dayGratisLimitMinor: metadosis_limit,
        lysisLimitMinor: totals.lysis_limit_minor,
        unusedLysisLimitMinor: totals.unused_minor,
        lysisAllocationMinor: totals.used_minor,
        promisLimitReturnedMinor: totals.unused_minor,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}

/// Reference currencies priced for `day`, filtered by the Nod entry bound.
fn priced_currencies(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<PricedCurrencies> {
    Ok(
        outbe_oracle::api::priced_reference_currencies_of_day(ctx.storage.clone(), day)?
            .into_iter()
            .filter(|&(_, vwap)| outbe_nod::pricing::is_issuable_entry(vwap))
            .collect(),
    )
}

fn seal_lysis(ctx: &BlockRuntimeContext<'_>, emission_day: &EmissionDay) -> Result<LysisTotals> {
    let wwd = emission_day.wwd;
    let mut tribute = TributeContract::new(ctx.storage.clone());
    let totals = match wwd.value() {
        0 => None,
        _ => Some(tribute.get_day_totals(wwd)?),
    };
    let Some(totals) = totals.filter(|totals| totals.tribute_count > 0) else {
        if wwd.value() != 0 {
            tribute.retire_completed_partition(wwd)?;
        }
        return Ok(LysisTotals::default());
    };
    tribute.freeze_day_nominal(&totals)?;
    let priced = priced_currencies(ctx, emission_day.day)?;
    let outcome = tribute.compute_and_record_metadosis_fractions(
        wwd,
        outbe_system_common::time::EmissionDate::new(emission_day.day),
        &priced,
        emission_day.metadosis_limit_minor(),
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), wwd, ctx.block.timestamp)?;
    Ok(LysisTotals::new(outcome))
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    let totals = seal_lysis(ctx, emission_day)?;
    let split = allocate_day_metadosis_limit(
        ctx.storage.clone(),
        LysisLimitInput {
            base_limit: emission_day.metadosis_limit_minor(),
            lysis_limit_minor: totals.lysis_limit_minor,
            nominal_total: totals.nominal_minor,
            unused_lysis_minor: totals.unused_minor,
        },
    )?;
    let desis_limit = brief_auction(ctx.storage.clone(), emission_day.day, &split)?;
    record_completion(metadosis, ctx, emission_day, &totals, desis_limit)
}
