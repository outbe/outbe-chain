//! Private READY classification and local settlement paths.

use alloy_primitives::U256;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::{
    aggregate::{WwdDayType, WwdProjection},
    commit::commit_outer_transition,
    constants::{RED_DAY_REDUCTION_COEF, SYMBOLIC_RATE},
    errors::MetadosisError,
    ocomp::schema::OcompRequestProfile,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, ReadyDisposition},
    schema::MetadosisContract,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadosisCalculation {
    pub(crate) gratis_demand: U256,
    pub(crate) day_gratis_limit_minor: U256,
    pub(crate) lysis_limit_minor: U256,
    pub(crate) desis_limit_minor: U256,
}

impl MetadosisContract<'_> {
    /// Core metadosis calculation for a worldwide day.
    pub(crate) fn calculate_metadosis(
        &self,
        wwd: WorldwideDay,
        tribute_nominal_total: U256,
        wwd_metadosis_limit: U256,
    ) -> Result<MetadosisCalculation> {
        let wwd_type = self.get_wwd_day_type(wwd)?;
        // Full-domain floor(total * SYMBOLIC_RATE / 100) without an
        // overflowing intermediate multiplication.
        let denominator = U256::from(100u64);
        let rate = U256::from(SYMBOLIC_RATE);
        let quotient = tribute_nominal_total / denominator;
        let remainder = tribute_nominal_total % denominator;
        let mut demand = quotient
            .checked_mul(rate)
            .and_then(|scaled| {
                remainder
                    .checked_mul(rate)
                    .and_then(|tail| scaled.checked_add(tail / denominator))
            })
            .ok_or_else(|| {
                crate::errors::storage_corruption("Metadosis full-precision demand overflow".into())
            })?;
        let mut day_gratis_limit_minor = wwd_metadosis_limit;
        match wwd_type {
            WwdDayType::Green => {}
            WwdDayType::Red => {
                demand /= U256::from(RED_DAY_REDUCTION_COEF);
                day_gratis_limit_minor /= U256::from(RED_DAY_REDUCTION_COEF);
            }
            WwdDayType::Unknown => {
                return Err(MetadosisError::UnknownWorldwideDayType.into());
            }
        }
        let lysis_limit_minor = demand.min(day_gratis_limit_minor);
        // The day sells what it earned beyond the symbolic share, and the limit
        // only caps it: the headroom a weak day leaves is not issued at all.
        let desis_limit_minor = tribute_nominal_total
            .min(wwd_metadosis_limit)
            .checked_sub(lysis_limit_minor)
            .ok_or_else(|| {
                crate::errors::storage_corruption(
                    "Metadosis Lysis Limit exceeds the day's nominal".into(),
                )
            })?;
        let split_total = lysis_limit_minor
            .checked_add(desis_limit_minor)
            .ok_or_else(|| crate::errors::storage_corruption("Metadosis split overflow".into()))?;
        if split_total > wwd_metadosis_limit {
            return Err(crate::errors::storage_corruption(
                "Metadosis split exceeds the day limit".into(),
            ));
        }
        Ok(MetadosisCalculation {
            gratis_demand: demand,
            day_gratis_limit_minor,
            lysis_limit_minor,
            desis_limit_minor,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalTerminalOutcome {
    ZeroDayLimit,
    UnknownDayType {
        day_limit: U256,
    },
    EmptyTributeDay {
        day_type: WwdDayType,
        day_limit: U256,
    },
    ZeroGratisAllocation {
        day_type: WwdDayType,
        tribute_nominal_total: U256,
        calculation: MetadosisCalculation,
    },
}

pub(crate) fn process_ocomp_ready_candidate(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    current: &WwdProjection,
    _profile: &OcompRequestProfile,
) -> Result<()> {
    let wwd = current.worldwide_day;
    let limit_amount = current.metadosis_limit_amount;
    if limit_amount.is_zero() {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            scope,
            current,
            LocalTerminalOutcome::ZeroDayLimit,
        );
    }
    let day_type = current.day_type;
    if day_type == WwdDayType::Unknown {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            scope,
            current,
            LocalTerminalOutcome::UnknownDayType {
                day_limit: limit_amount,
            },
        );
    }

    let tribute_totals = TributeContract::new(metadosis.storage.clone()).get_day_totals(wwd)?;
    if tribute_totals.tribute_count == 0 {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            scope,
            current,
            LocalTerminalOutcome::EmptyTributeDay {
                day_type,
                day_limit: limit_amount,
            },
        );
    }

    let calculation =
        metadosis.calculate_metadosis(wwd, tribute_totals.tribute_nominal_amount, limit_amount)?;
    if calculation.lysis_limit_minor.is_zero() {
        return process_local_terminal_outcome(
            metadosis,
            ctx,
            scope,
            current,
            LocalTerminalOutcome::ZeroGratisAllocation {
                day_type,
                tribute_nominal_total: tribute_totals.tribute_nominal_amount,
                calculation,
            },
        );
    }

    let transition = reduce_outer_wwd(
        Some(current),
        OuterWwdEvent::ProcessReady(ReadyDisposition::PrepareOcomp),
    )?;
    metadosis.initialize_ocomp_pre_admission(wwd)?;
    // This order is protocol-relevant: snapshot while CE is active, enqueue
    // the OCOMP FSM, then commit the outer transition.
    outbe_lysis::api::freeze_entry_price_snapshot(
        ctx.storage.clone(),
        wwd,
        current.scheduled_process_time,
    )?;
    metadosis.build_fidelity_league_snapshot(scope, parent, wwd, ctx.block.timestamp)?;
    metadosis.enqueue_ocomp_ready(wwd, ctx.block.block_number)?;
    commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)
}

fn process_local_terminal_outcome(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    current: &WwdProjection,
    outcome: LocalTerminalOutcome,
) -> Result<()> {
    let wwd = current.worldwide_day;
    let disposition = match outcome {
        LocalTerminalOutcome::ZeroDayLimit => ReadyDisposition::ZeroDayLimit,
        LocalTerminalOutcome::UnknownDayType { .. } => ReadyDisposition::UnknownDayType,
        LocalTerminalOutcome::EmptyTributeDay { .. } => ReadyDisposition::EmptyTributeDay,
        LocalTerminalOutcome::ZeroGratisAllocation { .. } => ReadyDisposition::ZeroGratisAllocation,
    };
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::ProcessReady(disposition))?;
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    match outcome {
        LocalTerminalOutcome::ZeroDayLimit => {
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            metadosis.emit(IMetadosis::MetadosisSkipped {
                worldwideDay: wwd.into(),
                reason: "day_metadosis_limit_is_zero".into(),
                status: "SKIPPED".into(),
                blockNumber: ctx.block.block_number,
            })
        }
        LocalTerminalOutcome::UnknownDayType { day_limit } => {
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            emit_failed_execution(metadosis, ctx, wwd, U256::ZERO, day_limit)?;
            promis_limit.add_to_total_unallocated(day_limit)
        }
        LocalTerminalOutcome::EmptyTributeDay {
            day_type,
            day_limit,
        } => {
            let to_promis = dispatch_brief(ctx, day_type, wwd, U256::ZERO)?;
            // A day with no tributes allocates nothing, so its whole limit stays on the warehouse.
            let returned = to_promis.checked_add(day_limit).ok_or_else(|| {
                crate::errors::storage_corruption("Metadosis day limit return overflow".into())
            })?;
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            TributeContract::new(metadosis.storage.clone())
                .retire_completed_partition(scope, wwd)?;
            metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
                worldwideDay: wwd.into(),
                dayMetadosisLimit: day_limit,
                dayMetadosisLimitRemainder: returned,
                status: "COMPLETED".into(),
                dayState: wwd_state_label(day_type).into(),
                action: "no tributes".into(),
            })?;
            promis_limit.add_to_total_unallocated(returned)
        }
        LocalTerminalOutcome::ZeroGratisAllocation {
            day_type,
            tribute_nominal_total,
            calculation,
        } => {
            let desis_limit_minor = calculation.desis_limit_minor;
            let to_promis = dispatch_brief(ctx, day_type, wwd, desis_limit_minor)?;
            // The limit headroom above the day's own nominal is issued by nobody, so it stays on
            // the warehouse together with whatever the brief did not take.
            let returned = current
                .metadosis_limit_amount
                .checked_sub(calculation.lysis_limit_minor)
                .and_then(|rest| rest.checked_sub(desis_limit_minor))
                .and_then(|headroom| headroom.checked_add(to_promis))
                .ok_or_else(|| {
                    crate::errors::storage_corruption(
                        "Metadosis split exceeds the day limit".into(),
                    )
                })?;
            promis_limit.add_to_total_unallocated(returned)?;
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            metadosis.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: wwd.into(),
                tributeTotals: tribute_nominal_total,
                dayGratisDemand: calculation.gratis_demand,
                dayGratisLimit: calculation.day_gratis_limit_minor,
                lysisLimitMinor: U256::ZERO,
                unusedLysisLimitMinor: U256::ZERO,
                lysisAllocationMinor: U256::ZERO,
                dayMetadosisLimitRemainder: returned,
                status: "COMPLETED".into(),
                blockNumber: ctx.block.block_number,
            })
        }
    }
}

fn dispatch_brief(
    ctx: &BlockRuntimeContext,
    dtype: WwdDayType,
    wwd: WorldwideDay,
    desis_limit_minor: U256,
) -> Result<U256> {
    // A day with nothing to sell is briefed as cancelled: an auction opened over
    // a zero limit would run its whole cross-chain cycle with no winner possible.
    let is_green = dtype == WwdDayType::Green && !desis_limit_minor.is_zero();
    let briefed_desis_limit_minor = if is_green {
        desis_limit_minor
    } else {
        U256::ZERO
    };
    let receipt = outbe_desis::api::dispatch_auction_brief(
        ctx.storage.clone(),
        wwd,
        briefed_desis_limit_minor,
        is_green,
        ctx.block.timestamp,
        outbe_desis::api::BriefOverflowPolicy::CarryOver,
    )?;
    match receipt {
        outbe_desis::api::AuctionBriefReceipt::Accepted => desis_limit_minor
            .checked_sub(briefed_desis_limit_minor)
            .ok_or_else(|| {
                crate::errors::storage_corruption(
                    "accepted Desis brief exceeds the Metadosis routing limit".into(),
                )
            }),
        outbe_desis::api::AuctionBriefReceipt::RejectedToCarryOver {
            reason: outbe_desis::api::AuctionBriefRejectionReason::SupplyExceedsAuctionDomain,
            desis_limit_minor: rejected_desis_limit_minor,
            max_accepted,
        } => {
            if rejected_desis_limit_minor != briefed_desis_limit_minor
                || rejected_desis_limit_minor <= max_accepted
            {
                return Err(crate::errors::storage_corruption(
                    "Desis rejection receipt does not match the dispatched limit".into(),
                ));
            }
            Ok(rejected_desis_limit_minor)
        }
    }
}

fn emit_failed_execution(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    tribute_totals: U256,
    day_metadosis_limit_remainder: U256,
) -> Result<()> {
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
        tributeTotals: tribute_totals,
        dayGratisDemand: U256::ZERO,
        dayGratisLimit: U256::ZERO,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        dayMetadosisLimitRemainder: day_metadosis_limit_remainder,
        status: "FAILED".into(),
        blockNumber: ctx.block.block_number,
    })
}

fn wwd_state_label(dtype: WwdDayType) -> &'static str {
    match dtype {
        WwdDayType::Green => "GREEN",
        WwdDayType::Red => "RED",
        WwdDayType::Unknown => "UNKNOWN",
    }
}
