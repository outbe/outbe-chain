//! Private READY classification and local settlement paths.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_desis::ReferencePrice;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
};
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
    pub(crate) gratis_supply: U256,
    pub(crate) gratis_allocation: U256,
    pub(crate) metadosis_limit_remainder: U256,
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
        let mut supply = wwd_metadosis_limit;
        match wwd_type {
            WwdDayType::Green => {}
            WwdDayType::Red => {
                demand /= U256::from(RED_DAY_REDUCTION_COEF);
                supply /= U256::from(RED_DAY_REDUCTION_COEF);
            }
            WwdDayType::Unknown => {
                return Err(MetadosisError::UnknownWorldwideDayType.into());
            }
        }
        let allocation = demand.min(supply);
        let remainder = wwd_metadosis_limit.checked_sub(allocation).ok_or_else(|| {
            crate::errors::storage_corruption("Metadosis allocation exceeds the day limit".into())
        })?;
        if allocation.checked_add(remainder) != Some(wwd_metadosis_limit) {
            return Err(crate::errors::storage_corruption(
                "Metadosis allocation conservation failed".into(),
            ));
        }
        Ok(MetadosisCalculation {
            gratis_demand: demand,
            gratis_supply: supply,
            gratis_allocation: allocation,
            metadosis_limit_remainder: remainder,
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
    profile: &OcompRequestProfile,
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
    if calculation.gratis_allocation.is_zero() {
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
    metadosis.build_fidelity_league_snapshot(scope, parent, wwd, ctx.block.timestamp)?;
    metadosis.enqueue_ocomp_ready(wwd, ctx.block.block_number, profile.fsm_limits())?;
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
            let to_promis = dispatch_brief(ctx, metadosis, day_type, wwd, day_limit)?;
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            TributeContract::new(metadosis.storage.clone())
                .retire_completed_partition(scope, wwd)?;
            metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
                worldwideDay: wwd.into(),
                dayMetadosisLimit: day_limit,
                dayMetadosisLimitRemainder: to_promis,
                status: "COMPLETED".into(),
                dayState: wwd_state_label(day_type).into(),
                action: "no tributes".into(),
            })?;
            promis_limit.add_to_total_unallocated(to_promis)
        }
        LocalTerminalOutcome::ZeroGratisAllocation {
            day_type,
            tribute_nominal_total,
            calculation,
        } => {
            let remainder = calculation.metadosis_limit_remainder;
            let to_promis = dispatch_brief(ctx, metadosis, day_type, wwd, remainder)?;
            promis_limit.add_to_total_unallocated(to_promis)?;
            commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number)?;
            metadosis.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: wwd.into(),
                tributeTotals: tribute_nominal_total,
                dayGratisDemand: calculation.gratis_demand,
                dayGratisLimit: calculation.gratis_supply,
                dayGratisAllocation: U256::ZERO,
                dayGratisAllocationRemainder: U256::ZERO,
                netDayGratisAllocation: U256::ZERO,
                dayMetadosisLimitRemainder: remainder,
                status: "COMPLETED".into(),
                blockNumber: ctx.block.block_number,
            })
        }
    }
}

fn dispatch_brief(
    ctx: &BlockRuntimeContext,
    metadosis: &mut MetadosisContract,
    dtype: WwdDayType,
    wwd: WorldwideDay,
    supply: U256,
) -> Result<U256> {
    let reference_prices = resolve_reference_entry_prices(metadosis, ctx, wwd)?;
    let is_green = dtype == WwdDayType::Green;
    let brief_supply = if is_green { supply } else { U256::ZERO };
    let receipt = outbe_desis::api::dispatch_auction_brief(
        ctx.storage.clone(),
        u32::from(wwd),
        brief_supply,
        reference_prices,
        is_green,
        ctx.block.timestamp,
    )?;
    match receipt {
        outbe_desis::api::AuctionBriefReceipt::Accepted => {
            supply.checked_sub(brief_supply).ok_or_else(|| {
                crate::errors::storage_corruption(
                    "accepted Desis brief exceeds Metadosis routing supply".into(),
                )
            })
        }
        outbe_desis::api::AuctionBriefReceipt::RejectedToCarryOver {
            reason: outbe_desis::api::AuctionBriefRejectionReason::SupplyExceedsAuctionDomain,
            supply: rejected_supply,
            max_accepted,
        } => {
            if rejected_supply != brief_supply || rejected_supply <= max_accepted {
                return Err(crate::errors::storage_corruption(
                    "Desis rejection receipt does not match the dispatched supply".into(),
                ));
            }
            Ok(rejected_supply)
        }
    }
}

/// One entry price per reference currency the oracle can price for the day: the
/// previous closed UTC day's VWAP of that currency's COEN pair, falling back to
/// the worldwide day's own. A currency the oracle cannot price is left out with
/// an event rather than failing the day, but at least one must survive.
fn resolve_reference_entry_prices(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<Vec<ReferencePrice>> {
    let last_closed = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let storage = metadosis.storage.clone();
    let mut rows = Vec::new();

    for iso_code in outbe_oracle::api::get_all_reference_currencies(ctx)? {
        let priced = match outbe_oracle::api::require_coen_pair(storage.clone(), iso_code) {
            Ok((_, index)) => resolve_pair_entry_price(storage.clone(), wwd, last_closed, index)?,
            Err(_) => None,
        };
        match priced {
            Some(entry_price_minor) => rows.push(ReferencePrice {
                iso_code,
                entry_price_minor,
            }),
            None => metadosis.emit(IMetadosis::ReferenceCurrencyUnpriced {
                worldwideDay: wwd.into(),
                isoCode: iso_code,
            })?,
        }
    }

    if rows.is_empty() {
        return Err(PrecompileError::Revert(
            "no reference currency has an entry price for the auction".into(),
        ));
    }
    Ok(rows)
}

fn resolve_pair_entry_price(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
    last_closed: u32,
    index: outbe_oracle::schema::PairIndex,
) -> Result<Option<U256>> {
    if let Some(vwap) = outbe_oracle::api::get_utc_day_vwap(storage.clone(), last_closed, index)? {
        if !vwap.is_zero() {
            return Ok(Some(vwap));
        }
    }
    Ok(
        outbe_oracle::api::get_worldwide_day_vwap_for_pair(storage, wwd, index)?
            .filter(|vwap| !vwap.is_zero()),
    )
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
        dayGratisAllocation: U256::ZERO,
        dayGratisAllocationRemainder: U256::ZERO,
        netDayGratisAllocation: U256::ZERO,
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
