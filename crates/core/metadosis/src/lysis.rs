//! Executes one sealed WorldwideDay linked to a due EmissionDay.

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::{
    lifecycle::complete_emission_day,
    limits::{apply_auction_brief, apply_lysis_limit, LysisLimitInput},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
    settlement::MetadosisCalculation,
};

fn mismatch() -> PrecompileError {
    PrecompileError::fatal("on-chain Lysis input or result mismatch")
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
    let split = apply_lysis_limit(
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
    let credit = PromisLimitContract::new(storage.clone()).checked_credit(completion.unused)?;
    if credit.credited != completion.unused {
        return Err(mismatch());
    }
    apply_auction_brief(storage, emission_day.day, &split)?;
    let promis_limit_added = emission_day
        .metadosis_limit_minor()
        .checked_sub(completion.used)
        .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            lysis_limit: calculation.lysis_limit_minor,
            desis_limit: split.desis_limit_minor,
            nods: completion.used,
            promis_limit_added,
        },
    )?;
    complete_emission_day(metadosis, &updated)?;
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

fn settle_partition(
    ctx: &BlockRuntimeContext<'_>,
    wwd: WorldwideDay,
    lysis_limit_minor: alloy_primitives::U256,
) -> Result<SettlementCompletion> {
    let mut tribute = TributeContract::new(ctx.storage.clone());
    let totals = match wwd.value() {
        0 => None,
        _ => Some(tribute.get_day_totals(wwd)?),
    };
    let Some(totals) = totals.filter(|totals| totals.tribute_count > 0) else {
        if wwd.value() != 0 {
            tribute.retire_completed_partition(wwd)?;
        }
        return Ok(SettlementCompletion {
            total_nominal_minor: alloy_primitives::U256::ZERO,
            used: alloy_primitives::U256::ZERO,
            unused: lysis_limit_minor,
        });
    };
    if !totals.is_sealed || !totals.initialized {
        return Err(mismatch());
    }
    tribute.freeze_day_nominal(&totals)?;
    let (_, onchain_allocated_minor) = tribute.compute_and_record_metadosis_fractions(
        wwd,
        lysis_limit_minor,
        ctx.block.timestamp,
    )?;
    outbe_fidelity::api::finalize_wwd_max_fidelity(ctx.storage.clone(), wwd, ctx.block.timestamp)?;
    let used = onchain_allocated_minor.min(lysis_limit_minor);
    Ok(SettlementCompletion {
        total_nominal_minor: totals.tribute_nominal_total_minor,
        used,
        unused: lysis_limit_minor.saturating_sub(used),
    })
}

pub(crate) fn execute(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
    calculation: MetadosisCalculation,
) -> Result<()> {
    ctx.storage.require_metadosis_process_ready()?;
    let completion = settle_partition(ctx, emission_day.wwd, calculation.lysis_limit_minor)?;
    finalize_lysis_settlement(metadosis, ctx, emission_day, calculation, completion)
}
