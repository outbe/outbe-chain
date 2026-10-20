//! READY EmissionDay classification and local settlement paths.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::{
    constants::SYMBOLIC_RATE,
    lifecycle::{complete_emission_day, fail_emission_day},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadosisCalculation {
    pub(crate) gratis_demand: U256,
    pub(crate) day_gratis_limit_minor: U256,
    pub(crate) lysis_limit_minor: U256,
}

impl MetadosisContract<'_> {
    /// Core metadosis calculation for an EmissionDay.
    pub(crate) fn calculate_metadosis(
        &self,
        tribute_nominal_total: U256,
        emission_limit_minor: U256,
    ) -> Result<MetadosisCalculation> {
        let denominator = U256::from(100u64);
        let rate = U256::from(SYMBOLIC_RATE);
        let quotient = tribute_nominal_total / denominator;
        let remainder = tribute_nominal_total % denominator;
        let demand = quotient
            .checked_mul(rate)
            .and_then(|scaled| {
                remainder
                    .checked_mul(rate)
                    .and_then(|tail| scaled.checked_add(tail / denominator))
            })
            .ok_or(crate::errors::MetadosisStorageCorruptionError::FullPrecisionDemandOverflow)?;
        let day_gratis_limit_minor = emission_limit_minor;
        let lysis_limit_minor = demand.min(day_gratis_limit_minor);
        Ok(MetadosisCalculation {
            gratis_demand: demand,
            day_gratis_limit_minor,
            lysis_limit_minor,
        })
    }
}

/// The Desis Limit of a day: what the day earned beyond the Lysis share. The cap is what Lysis left
/// of the day's own limit plus what the path draws from the carry-over.
pub(crate) fn desis_limit(
    nominal_total: U256,
    lysis_limit_minor: U256,
    day_limit: U256,
    carry_over_draw: U256,
) -> Option<U256> {
    let available = day_limit
        .checked_sub(lysis_limit_minor)?
        .checked_add(carry_over_draw)?;
    Some(nominal_total.checked_sub(lysis_limit_minor)?.min(available))
}

pub(crate) fn process_ready_candidate(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let limit_amount = emission_day.emission_limit_minor;
    if limit_amount.is_zero() {
        return fail_emission_day(
            metadosis,
            ctx,
            emission_day.day,
            wwd,
            "emission limit is zero",
        );
    }

    let mut tribute = TributeContract::new(metadosis.storage.clone());
    let mut tribute_totals = tribute.get_day_totals(wwd)?;
    if !tribute_totals.is_sealed {
        tribute.seal_day(wwd)?;
        tribute_totals = tribute.get_day_totals(wwd)?;
    }

    if tribute_totals.tribute_count == 0 {
        return settle_empty_day(metadosis, ctx, emission_day);
    }

    let calculation =
        metadosis.calculate_metadosis(tribute_totals.tribute_nominal_total_minor, limit_amount)?;
    if calculation.lysis_limit_minor.is_zero() {
        return settle_zero_allocation(
            metadosis,
            ctx,
            emission_day,
            tribute_totals.tribute_nominal_total_minor,
            calculation,
        );
    }

    crate::lysis::execute(metadosis, ctx, emission_day, calculation)
}

fn settle_empty_day(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    emission_day: &EmissionDay,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let day_limit = emission_day.emission_limit_minor;
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    let to_promis = dispatch_brief(ctx, wwd, U256::ZERO)?;
    let returned = to_promis
        .checked_add(day_limit)
        .ok_or(crate::errors::MetadosisStorageCorruptionError::DayLimitReturnOverflow)?;
    complete_emission_day(metadosis, emission_day, emission_day_state::SUCCEEDED)?;
    TributeContract::new(metadosis.storage.clone()).retire_completed_partition(wwd)?;
    metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
        worldwideDay: wwd.into(),
        metadosisLimitMinor: day_limit,
        promisLimitReturnedMinor: returned,
        status: "COMPLETED".into(),
        action: "no tributes".into(),
    })?;
    promis_limit.add_to_total_unallocated(returned)
}

fn settle_zero_allocation(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    emission_day: &EmissionDay,
    tribute_nominal_total: U256,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    let desis_limit_minor = desis_limit(
        tribute_nominal_total,
        calculation.lysis_limit_minor,
        emission_day.emission_limit_minor,
        U256::ZERO,
    )
    .ok_or(crate::errors::MetadosisStorageCorruptionError::LysisLimitExceedsDayNominal)?;
    let to_promis = dispatch_brief(ctx, wwd, desis_limit_minor)?;
    let returned = emission_day
        .emission_limit_minor
        .checked_sub(calculation.lysis_limit_minor)
        .and_then(|rest| rest.checked_sub(desis_limit_minor))
        .and_then(|headroom| headroom.checked_add(to_promis))
        .ok_or(crate::errors::MetadosisStorageCorruptionError::SplitExceedsDayLimit)?;
    promis_limit.add_to_total_unallocated(returned)?;
    complete_emission_day(metadosis, emission_day, emission_day_state::SUCCEEDED)?;
    TributeContract::new(metadosis.storage.clone()).forfeit_sealed_partition(wwd)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
        tributeNominalTotalMinor: tribute_nominal_total,
        gratisDemandMinor: calculation.gratis_demand,
        dayGratisLimitMinor: calculation.day_gratis_limit_minor,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        promisLimitReturnedMinor: returned,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}

fn dispatch_brief(
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    desis_limit_minor: U256,
) -> Result<U256> {
    let is_active = !desis_limit_minor.is_zero();
    let briefed_desis_limit_minor = if is_active {
        desis_limit_minor
    } else {
        U256::ZERO
    };
    let receipt = outbe_desis::api::dispatch_auction_brief(
        ctx.storage.clone(),
        outbe_desis::api::AuctionBrief {
            worldwide_day: wwd,
            desis_limit_minor: briefed_desis_limit_minor,
            has_limit: is_active,
        },
        ctx.block.timestamp,
        outbe_desis::api::BriefOverflowPolicy::CarryOver,
    )?;
    match receipt {
        outbe_desis::api::AuctionBriefReceipt::Accepted => desis_limit_minor
            .checked_sub(briefed_desis_limit_minor)
            .ok_or_else(|| {
                crate::errors::MetadosisStorageCorruptionError::AcceptedDesisBriefExceedsRoutingLimit
                    .into()
            }),
        outbe_desis::api::AuctionBriefReceipt::RejectedToCarryOver {
            reason: outbe_desis::api::AuctionBriefRejectionReason::DesisLimitExceedsAuctionDomain,
            desis_limit_minor: rejected_desis_limit_minor,
            max_accepted,
        } => {
            if rejected_desis_limit_minor != briefed_desis_limit_minor
                || rejected_desis_limit_minor <= max_accepted
            {
                return Err(
                    crate::errors::MetadosisStorageCorruptionError::DesisRejectionReceiptMismatchDispatchedLimit
                        .into(),
                );
            }
            Ok(rejected_desis_limit_minor)
        }
    }
}
