//! Executes one sealed WorldwideDay linked to a due EmissionDay.

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{PricedCurrencies, TributeContract};

use crate::{
    lifecycle::complete_metadosis,
    limits::{allocate_day_metadosis_limit, apply_auction_brief, LysisLimitInput},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
    settlement::MetadosisCalculation,
};

fn mismatch() -> PrecompileError {
    crate::errors::MetadosisError::LysisResultMismatch.into()
}

struct SettlementCompletion {
    total_nominal_minor: alloy_primitives::U256,
    used: alloy_primitives::U256,
    unused: alloy_primitives::U256,
}

fn finalize_lysis_settlement(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
    completion: SettlementCompletion,
) -> Result<()> {
    let storage = ctx.storage.clone();
    let split = allocate_day_metadosis_limit(
        storage.clone(),
        LysisLimitInput {
            base_limit: emission_day.metadosis_limit_minor(),
            lysis_limit_minor: calculation.lysis_limit_minor,
            nominal_total: completion.total_nominal_minor,
        },
    )?;
    if completion
        .used
        .checked_add(split.desis_limit_minor)
        .is_none_or(|allocation| allocation > completion.total_nominal_minor)
    {
        return Err(mismatch());
    }
    PromisLimitContract::new(storage.clone()).checked_credit(completion.unused)?;
    let desis_limit = apply_auction_brief(storage, emission_day.day, &split)?;
    let promis_limit_added = emission_day
        .metadosis_limit_minor()
        .checked_sub(completion.used)
        .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            lysis_limit: calculation.lysis_limit_minor,
            desis_limit,
            nods: completion.used,
            promis_limit_added,
        },
    )?;
    complete_metadosis(metadosis, &updated)?;
    metadosis.emit(IMetadosis::MetadosisCompleted {
        emissionDay: emission_day.day,
        tributeNominalTotalMinor: completion.total_nominal_minor,
        gratisDemandMinor: calculation.gratis_demand,
        dayGratisLimitMinor: calculation.day_gratis_limit_minor,
        lysisLimitMinor: calculation.lysis_limit_minor,
        unusedLysisLimitMinor: completion.unused,
        lysisAllocationMinor: completion.used,
        promisLimitReturnedMinor: completion.unused,
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

fn settle_partition(
    metadosis: &MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<(MetadosisCalculation, SettlementCompletion)> {
    let wwd = emission_day.wwd;
    let metadosis_limit = emission_day.metadosis_limit_minor();
    let mut tribute = TributeContract::new(ctx.storage.clone());
    let totals = match wwd.value() {
        0 => None,
        _ => Some(tribute.get_day_totals(wwd)?),
    };
    let Some(totals) = totals.filter(|totals| totals.tribute_count > 0) else {
        if wwd.value() != 0 {
            tribute.retire_completed_partition(wwd)?;
        }
        let calculation = metadosis.lysis_limit_of_day(U256::ZERO, metadosis_limit)?;
        return Ok((
            calculation,
            SettlementCompletion {
                total_nominal_minor: U256::ZERO,
                used: U256::ZERO,
                unused: calculation.lysis_limit_minor,
            },
        ));
    };
    tribute.freeze_day_nominal(&totals)?;
    let priced = priced_currencies(ctx, emission_day.day)?;
    let outcome = tribute.compute_and_record_metadosis_fractions(
        wwd,
        &priced,
        metadosis_limit,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), wwd, ctx.block.timestamp)?;
    let calculation = MetadosisCalculation {
        gratis_demand: outcome.gratis_demand_minor,
        day_gratis_limit_minor: metadosis_limit,
        lysis_limit_minor: outcome.lysis_limit_minor,
    };
    let used = outcome.allocated_minor.min(calculation.lysis_limit_minor);
    Ok((
        calculation,
        SettlementCompletion {
            total_nominal_minor: outcome.nominal_minor,
            used,
            unused: calculation.lysis_limit_minor.saturating_sub(used),
        },
    ))
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    let (calculation, completion) = settle_partition(metadosis, ctx, emission_day)?;
    finalize_lysis_settlement(metadosis, ctx, emission_day, calculation, completion)
}
