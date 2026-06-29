//! The Metadosis settlement as an `smlang` FSM.
//!
//! Settlement of a READY WorldwideDay follows the flow
//! `limit → lysis → auction clearing (intex/desis) → terminal`, modeled as states:
//!
//! ```text
//! Pending ─[has_tributes]/run_lysis─▶ Lysed ─clear_and_emit─▶ Cleared ─settle─▶ Settled
//!    │  └─[no_tributes]/clear_empty──────────────────────────▶ Cleared
//!    ├─[limit==0]/reject_zero_limit──▶ Aborted
//!    └─[unknown]/reject_unknown──────▶ Aborted
//! ```
//!
//! Guards are cheap `&self` reads of values materialized once in
//! [`execute_metadosis`] (limit, day type, tribute count); all heavy/fallible money
//! work lives in `&mut self` actions returning `PrecompileError`. A lysis failure is
//! genuine corruption: its action emits the FAILED record and returns `Err`, which
//! `process_event` surfaces as `ActionFailed` → the caller settles the day FAILED
//! and reverts the block.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result as MdResult},
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;
use smlang::statemachine;

use crate::constants::{RED_DAY_REDUCTION_COEF, SYMBOLIC_RATE};
// Aliased: the bare `MetadosisError` in this module is the smlang-generated
// transition error (`name: Metadosis`), not the domain error enum.
use crate::errors::MetadosisError as MetadosisDomainError;
use crate::precompile::IMetadosis;
use crate::schema::{DayType, MetadosisContract, Status, WorldwideDayEntryExt};

/// Result of the pure gratis-allocation formula ([`compute_allocation`]).
pub(crate) struct MetadosisCalculation {
    gratis_demand: U256,
    gratis_supply: U256,
    gratis_allocation: U256,
    metadosis_limit_remainder: U256,
}

/// The gratis-allocation formula for one settled day. **Pure** — it operates on the
/// already-materialized day type, tribute total, and metadosis limit (no storage),
/// so it is unit-testable in isolation. RED days halve both demand and supply, and
/// the allocation is capped at the day's limit (`min(demand, supply)`).
pub(crate) fn compute_allocation(
    wwd_type: DayType,
    tribute_nominal_total: U256,
    wwd_metadosis_limit: U256,
) -> MdResult<MetadosisCalculation> {
    let mut demand = tribute_nominal_total * U256::from(SYMBOLIC_RATE) / U256::from(100u64);
    let mut supply = wwd_metadosis_limit;

    match wwd_type {
        DayType::Green => {}
        DayType::Red => {
            demand /= U256::from(RED_DAY_REDUCTION_COEF);
            supply /= U256::from(RED_DAY_REDUCTION_COEF);
        }
        DayType::Unknown => return Err(MetadosisDomainError::UnknownWorldwideDayType.into()),
    }

    let allocation = if demand < supply { demand } else { supply };
    let metadosis_limit_remainder = wwd_metadosis_limit - allocation;

    Ok(MetadosisCalculation {
        gratis_demand: demand,
        gratis_supply: supply,
        gratis_allocation: allocation,
        metadosis_limit_remainder,
    })
}

/// The lysis allocation result, carried as `Lysed`'s state data from `on_run_lysis`
/// to `on_clear_and_emit` — the intermediate values flow through the state, not a
/// mutable context bag.
#[derive(Debug, Clone, Copy)]
pub struct LysisOutput {
    gratis_demand: U256,
    gratis_supply: U256,
    gratis_allocation: U256,
    lysis_remaining_gratis: U256,
    remainder: U256,
}

statemachine! {
    name: Metadosis,
    custom_error: true,
    derive_states: [Debug, Clone, Copy],
    transitions: {
        *Pending + Resolve [limit_is_zero] / on_reject_zero_limit = Aborted,
        Pending  + Resolve [day_unknown]   / on_reject_unknown    = Aborted,
        Pending  + Resolve [no_tributes]   / on_clear_empty       = Cleared,
        Pending  + Resolve [has_tributes]  / on_run_lysis         = Lysed(LysisOutput),
        Lysed(LysisOutput) + ClearAuction  / on_clear_and_emit    = Cleared,
        Cleared  + Settle                                         = Settled,
    },
}

/// Machine context: the scoped block runtime + the day and the values the guards
/// branch on (materialized once). The lysis path carries its intermediate result
/// as `Lysed`'s state data, not through mutable context scratch.
pub struct MetadosisCtx<'a, 'storage> {
    rt: &'a BlockRuntimeContext<'storage>,
    wwd: WorldwideDay,
    limit_amount: U256,
    wwd_type: DayType,
    tribute_count: u32,
    tribute_nominal_total: U256,
}

impl<'storage> MetadosisCtx<'_, 'storage> {
    /// Short-lived Metadosis contract facade for one guard/action — an `Rc` clone
    /// of the block's storage handle, never held across calls.
    fn md(&self) -> MetadosisContract<'storage> {
        MetadosisContract::new(self.rt.storage.clone())
    }

    fn auction_ts(&self) -> MdResult<u64> {
        self.md()
            .worldwide_days
            .entry(self.wwd)
            .scheduled_process_time()
            .read()
    }
}

impl MetadosisStateMachineContext for MetadosisCtx<'_, '_> {
    type Error = PrecompileError;

    // ---- guards: cheap predicates over already-materialized values ----------
    fn limit_is_zero(&self) -> MdResult<bool> {
        Ok(self.limit_amount.is_zero())
    }
    fn day_unknown(&self) -> MdResult<bool> {
        Ok(self.wwd_type == DayType::Unknown)
    }
    fn no_tributes(&self) -> MdResult<bool> {
        Ok(self.tribute_count == 0)
    }
    fn has_tributes(&self) -> MdResult<bool> {
        Ok(self.tribute_count > 0)
    }

    // ---- abort branches -----------------------------------------------------
    fn on_reject_zero_limit(&mut self) -> MdResult<()> {
        let ts = self.auction_ts()?;
        dispatch_auction_clearing(self.rt, self.wwd_type, ts, U256::ZERO)?;
        self.md().emit(IMetadosis::MetadosisSkipped {
            worldwideDay: self.wwd.into(),
            reason: "day_metadosis_limit_is_zero".into(),
            status: "SKIPPED".into(),
            blockNumber: self.rt.block.block_number,
        })
    }

    fn on_reject_unknown(&mut self) -> MdResult<()> {
        let mut md = self.md();
        emit_failed_execution(&mut md, self.rt, self.wwd, U256::ZERO, self.limit_amount)?;
        PromisLimitContract::new(self.rt.storage.clone())
            .add_to_total_unallocated(self.limit_amount)
    }

    // ---- no-tributes branch: close the GREEN auction, route remainder -------
    fn on_clear_empty(&mut self) -> MdResult<()> {
        let ts = self.auction_ts()?;
        let to_promis = dispatch_auction_clearing(self.rt, self.wwd_type, ts, self.limit_amount)?;
        self.md().emit(IMetadosis::MetadosisWorldwideDayProcessed {
            worldwideDay: self.wwd.into(),
            dayMetadosisLimit: self.limit_amount,
            dayMetadosisLimitRemainder: to_promis,
            status: Status::Completed.label().into(),
            dayState: self.wwd_type.label().into(),
            action: "no tributes".into(),
        })?;
        PromisLimitContract::new(self.rt.storage.clone()).add_to_total_unallocated(to_promis)
    }

    // ---- lysis branch: allocate gratis; on failure emit + revert ------------
    fn on_run_lysis(&mut self) -> MdResult<LysisOutput> {
        let params =
            compute_allocation(self.wwd_type, self.tribute_nominal_total, self.limit_amount)?;
        match outbe_lysis::runtime::lysis(
            self.rt.storage.clone(),
            self.wwd,
            params.gratis_allocation,
        ) {
            Ok(result) => Ok(LysisOutput {
                gratis_demand: params.gratis_demand,
                gratis_supply: params.gratis_supply,
                gratis_allocation: params.gratis_allocation,
                lysis_remaining_gratis: result.remaining_gratis,
                // Both terms are bounded by the day metadosis limit, so the sum
                // cannot overflow U256; saturating keeps the consensus path
                // panic-free (mirrors the `saturating_sub` in `on_clear_and_emit`).
                remainder: result
                    .remaining_gratis
                    .saturating_add(params.metadosis_limit_remainder),
            }),
            Err(err) => {
                // Genuine corruption (e.g. NOD-id collision): record + propagate.
                let mut md = self.md();
                emit_failed_execution(
                    &mut md,
                    self.rt,
                    self.wwd,
                    self.tribute_nominal_total,
                    self.limit_amount,
                )?;
                Err(err)
            }
        }
    }

    // ---- lysis branch: clear the whole PROMIS pool + emit result ------------
    fn on_clear_and_emit(&mut self, lysis: &LysisOutput) -> MdResult<()> {
        let mut promis_limit = PromisLimitContract::new(self.rt.storage.clone());
        promis_limit.add_to_total_unallocated(lysis.remainder)?;
        let promis_total_unallocated = promis_limit.get_total_unallocated()?;
        let ts = self.auction_ts()?;
        let clearing_reminder =
            dispatch_auction_clearing(self.rt, self.wwd_type, ts, promis_total_unallocated)?;
        promis_limit.set_total_unallocated(clearing_reminder)?;

        self.md().emit(IMetadosis::MetadosisExecuted {
            worldwideDay: self.wwd.into(),
            tributeTotals: self.tribute_nominal_total,
            dayGratisDemand: lysis.gratis_demand,
            dayGratisLimit: lysis.gratis_supply,
            dayGratisAllocation: lysis.gratis_allocation,
            dayGratisAllocationRemainder: lysis.lysis_remaining_gratis,
            netDayGratisAllocation: lysis
                .gratis_allocation
                .saturating_sub(lysis.lysis_remaining_gratis),
            dayMetadosisLimitRemainder: lysis.remainder,
            status: Status::Completed.label().into(),
            blockNumber: self.rt.block.block_number,
        })
    }
}

fn map_smlang_err(e: MetadosisError<PrecompileError>) -> PrecompileError {
    match e {
        MetadosisError::GuardFailed(err) | MetadosisError::ActionFailed(err) => err,
        other => PrecompileError::Revert(format!("metadosis transition error: {other:?}")),
    }
}

/// Run the metadosis settlement for an IN_PROGRESS day and advance the Metadosis
/// machine through its flow to a terminal (`Settled`/`Aborted`). A lysis corruption
/// returns `Err` (the caller settles FAILED and reverts). The branch values (limit,
/// day type, tribute totals) are read once and handed to the machine context.
pub(crate) fn execute_metadosis(
    rt: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> MdResult<MetadosisStates> {
    let md = MetadosisContract::new(rt.storage.clone());
    let limit_amount = md
        .worldwide_days
        .entry(wwd)
        .metadosis_limit_amount()
        .read()?;
    let wwd_type = DayType::try_from(md.get_wwd_day_type(wwd)?)?;
    let tribute_day_totals = TributeContract::new(rt.storage.clone()).get_day_totals(wwd)?;

    let ctx = MetadosisCtx {
        rt,
        wwd,
        limit_amount,
        wwd_type,
        tribute_count: tribute_day_totals.tribute_count,
        tribute_nominal_total: tribute_day_totals.tribute_nominal_amount,
    };

    let mut machine = MetadosisStateMachine::new(ctx);
    machine
        .process_event(MetadosisEvents::Resolve)
        .map_err(map_smlang_err)?;
    loop {
        let state = *machine.state();
        match state {
            MetadosisStates::Lysed(_) => {
                machine
                    .process_event(MetadosisEvents::ClearAuction)
                    .map_err(map_smlang_err)?;
            }
            MetadosisStates::Cleared => {
                machine
                    .process_event(MetadosisEvents::Settle)
                    .map_err(map_smlang_err)?;
            }
            MetadosisStates::Settled | MetadosisStates::Aborted => break,
            MetadosisStates::Pending => {
                return Err(PrecompileError::Revert(
                    "metadosis settlement stuck at Pending".into(),
                ))
            }
        }
    }
    Ok(*machine.state())
}

fn dispatch_auction_clearing(
    rt: &BlockRuntimeContext,
    dtype: DayType,
    auction_ts: u64,
    supply: U256,
) -> MdResult<U256> {
    if dtype != DayType::Green {
        return Ok(supply);
    }
    // Returns the PROMIS remainder the auction could not consume: the rounding
    // remainder on a delivered clearing, or the whole `supply` on a best-effort
    // Desis failure. The caller writes this back into the PromisLimit accumulator.
    outbe_desis::api::dispatch_stage_clearing(rt.storage.clone(), auction_ts, supply)
}

fn emit_failed_execution(
    md: &mut MetadosisContract,
    rt: &BlockRuntimeContext,
    wwd: WorldwideDay,
    tribute_totals: U256,
    day_metadosis_limit_remainder: U256,
) -> MdResult<()> {
    md.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
        tributeTotals: tribute_totals,
        dayGratisDemand: U256::ZERO,
        dayGratisLimit: U256::ZERO,
        dayGratisAllocation: U256::ZERO,
        dayGratisAllocationRemainder: U256::ZERO,
        netDayGratisAllocation: U256::ZERO,
        dayMetadosisLimitRemainder: day_metadosis_limit_remainder,
        status: Status::Failed.label().into(),
        blockNumber: rt.block.block_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The gratis-allocation formula is now pure: tested with plain values, no
    // storage / contract setup (was `tests/state.rs::test_calculate_metadosis_*`).

    #[test]
    fn compute_allocation_green_caps_at_limit() {
        // SYMBOLIC_RATE = 32, GREEN: demand = 10_000*32/100 = 3_200; limit = 5_000;
        // allocation = min(3_200, 5_000) = 3_200; remainder = 5_000 - 3_200 = 1_800.
        let c = compute_allocation(DayType::Green, U256::from(10_000u64), U256::from(5_000u64))
            .unwrap();
        assert_eq!(c.gratis_allocation, U256::from(3_200u64));
        assert_eq!(c.metadosis_limit_remainder, U256::from(1_800u64));
    }

    #[test]
    fn compute_allocation_red_halves_demand_and_supply() {
        // RED (RED_DAY_REDUCTION_COEF = 8): demand = 10_000*32/100/8 = 400;
        // supply = 5_000/8 = 625; allocation = min(400, 625) = 400;
        // remainder = 5_000 - 400 = 4_600 (against the full limit, not the halved supply).
        let c =
            compute_allocation(DayType::Red, U256::from(10_000u64), U256::from(5_000u64)).unwrap();
        assert_eq!(c.gratis_allocation, U256::from(400u64));
        assert_eq!(c.metadosis_limit_remainder, U256::from(4_600u64));
    }

    #[test]
    fn compute_allocation_unknown_day_type_errors() {
        assert!(compute_allocation(
            DayType::Unknown,
            U256::from(10_000u64),
            U256::from(5_000u64)
        )
        .is_err());
    }
}
