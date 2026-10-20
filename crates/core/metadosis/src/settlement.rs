//! Private READY classification and local settlement paths.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::{
    aggregate::WwdProjection,
    commit::commit_outer_transition,
    constants::SYMBOLIC_RATE,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, ReadyDisposition},
    schema::MetadosisContract,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadosisCalculation {
    pub(crate) gratis_demand: U256,
    pub(crate) day_gratis_limit_minor: U256,
    pub(crate) lysis_limit_minor: U256,
}

/// Core metadosis calculation for a worldwide day. Every day uses the same rule: the Lysis
/// limit is `min(floor(nominal * SYMBOLIC_RATE / 100), day limit)`.
pub(crate) fn calculate_metadosis(
    tribute_nominal_total: U256,
    wwd_metadosis_limit: U256,
) -> Result<MetadosisCalculation> {
    // Full-domain floor(total * SYMBOLIC_RATE / 100) without an
    // overflowing intermediate multiplication.
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
    let day_gratis_limit_minor = wwd_metadosis_limit;
    let lysis_limit_minor = demand.min(day_gratis_limit_minor);
    Ok(MetadosisCalculation {
        gratis_demand: demand,
        day_gratis_limit_minor,
        lysis_limit_minor,
    })
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalTerminalOutcome {
    EmptyTributeDay {
        day_limit: U256,
    },
    ZeroGratisAllocation {
        tribute_nominal_total: U256,
        calculation: MetadosisCalculation,
    },
}

pub(crate) fn process_ready_candidate(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
) -> Result<()> {
    let wwd = current.worldwide_day;
    let limit_amount = current.metadosis_limit_minor;
    if limit_amount.is_zero() {
        return crate::terminal::fail_worldwide_day(
            ctx.storage.clone(),
            ctx.block.block_number,
            wwd,
        );
    }

    let tribute_totals = TributeContract::new(metadosis.storage.clone()).get_day_totals(wwd)?;
    if tribute_totals.tribute_count == 0 {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            current,
            LocalTerminalOutcome::EmptyTributeDay {
                day_limit: limit_amount,
            },
        );
    }

    let calculation =
        calculate_metadosis(tribute_totals.tribute_nominal_total_minor, limit_amount)?;
    if calculation.lysis_limit_minor.is_zero() {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            current,
            LocalTerminalOutcome::ZeroGratisAllocation {
                tribute_nominal_total: tribute_totals.tribute_nominal_total_minor,
                calculation,
            },
        );
    }

    crate::lysis::execute(metadosis, ctx, current, calculation)
}

struct LocalSettlement<'effect, 'storage> {
    ctx: &'effect BlockRuntimeContext<'storage>,

    current: &'effect WwdProjection,
    transition: &'effect crate::reducer::OuterWwdTransition,
}

fn process_local_terminal_outcome(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    current: &WwdProjection,
    outcome: LocalTerminalOutcome,
) -> Result<()> {
    let disposition = match outcome {
        LocalTerminalOutcome::EmptyTributeDay { .. } => ReadyDisposition::EmptyTributeDay,
        LocalTerminalOutcome::ZeroGratisAllocation { .. } => ReadyDisposition::ZeroGratisAllocation,
    };
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::ProcessReady(disposition))?;
    let settlement = LocalSettlement {
        ctx,
        current,
        transition: &transition,
    };
    match outcome {
        LocalTerminalOutcome::EmptyTributeDay { day_limit } => {
            settle_empty_day(metadosis, settlement, day_limit)
        }
        LocalTerminalOutcome::ZeroGratisAllocation {
            tribute_nominal_total,
            calculation,
        } => settle_zero_allocation(
            metadosis,
            settlement,
            ZeroAllocation {
                tribute_nominal_total,
                calculation,
            },
        ),
    }
}

fn settle_empty_day(
    metadosis: &mut MetadosisContract,
    settlement: LocalSettlement<'_, '_>,
    day_limit: U256,
) -> Result<()> {
    let LocalSettlement {
        ctx,
        current,
        transition,
    } = settlement;
    let wwd = current.worldwide_day;
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    let to_promis = dispatch_brief(ctx, wwd, U256::ZERO)?;
    // A day with no tributes allocates nothing, so its whole limit stays on the warehouse.
    let returned = to_promis
        .checked_add(day_limit)
        .ok_or(crate::errors::MetadosisStorageCorruptionError::DayLimitReturnOverflow)?;
    commit_outer_transition(metadosis, wwd, transition, ctx.block.block_number)?;
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

struct ZeroAllocation {
    tribute_nominal_total: U256,
    calculation: MetadosisCalculation,
}

fn settle_zero_allocation(
    metadosis: &mut MetadosisContract,
    settlement: LocalSettlement<'_, '_>,
    allocation: ZeroAllocation,
) -> Result<()> {
    let LocalSettlement {
        ctx,
        current,
        transition,
    } = settlement;
    let ZeroAllocation {
        tribute_nominal_total,
        calculation,
    } = allocation;
    let wwd = current.worldwide_day;
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    // The local path never draws from the carry-over: its auction sells from the day alone.
    let desis_limit_minor = desis_limit(
        tribute_nominal_total,
        calculation.lysis_limit_minor,
        current.metadosis_limit_minor,
        U256::ZERO,
    )
    .ok_or(crate::errors::MetadosisStorageCorruptionError::LysisLimitExceedsDayNominal)?;
    let to_promis = dispatch_brief(ctx, wwd, desis_limit_minor)?;
    // Nobody issues the limit headroom above the day's own nominal, so it stays on the
    // warehouse together with whatever the brief did not take.
    let returned = current
        .metadosis_limit_minor
        .checked_sub(calculation.lysis_limit_minor)
        .and_then(|rest| rest.checked_sub(desis_limit_minor))
        .and_then(|headroom| headroom.checked_add(to_promis))
        .ok_or(crate::errors::MetadosisStorageCorruptionError::SplitExceedsDayLimit)?;
    promis_limit.add_to_total_unallocated(returned)?;
    commit_outer_transition(metadosis, wwd, transition, ctx.block.block_number)?;
    // No Lysis allocation consumes these tributes, so this path forfeits the sealed partition.
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
    // A day with nothing to sell is briefed with no limit, and Desis cancels it. An auction
    // opened over a zero limit would run its whole cross-chain cycle with no winner possible.
    let has_limit = !desis_limit_minor.is_zero();
    let receipt = outbe_desis::api::dispatch_auction_brief(
        ctx.storage.clone(),
        outbe_desis::api::AuctionBrief {
            worldwide_day: wwd,
            desis_limit_minor,
            has_limit,
        },
        ctx.block.timestamp,
        outbe_desis::api::BriefOverflowPolicy::CarryOver,
    )?;
    match receipt {
        // An accepted brief takes the whole limit, so nothing goes back to the carry-over.
        outbe_desis::api::AuctionBriefReceipt::Accepted => Ok(U256::ZERO),
        outbe_desis::api::AuctionBriefReceipt::RejectedToCarryOver {
            reason: outbe_desis::api::AuctionBriefRejectionReason::DesisLimitExceedsAuctionDomain,
            desis_limit_minor: rejected_desis_limit_minor,
            max_accepted,
        } => {
            if rejected_desis_limit_minor != desis_limit_minor
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
