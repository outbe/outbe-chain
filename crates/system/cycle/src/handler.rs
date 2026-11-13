//! ProtocolCycle calendar orchestration and completed-day emission settlement.
//!
//! This is the natural home of the `Cycle -> EmissionLimit -> AgentReward
//! -> Rewards -> Metadosis` orchestration. The orchestration lives in `outbe-cycle`
//! (rather than `outbe-emissionlimit`) to avoid a `outbe-emissionlimit -> outbe-rewards`
//! dependency edge. That edge would close the cycle with the existing
//! `outbe-rewards -> outbe-emissionlimit` re-export.

use alloy_primitives::U256;
use outbe_agentreward::api::{allocate_day_agent_rewards, process_agent_rewards, DayAgentPools};
use outbe_emission::{
    allocation::{allocate_day_emission_limit, EmissionSinkId},
    api::DayAllocationAmounts,
    day_emission::emission_limit_of_day,
    schema::emission_day_state,
};
use outbe_metadosis::commands::set_metadosis_limit;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::{
        date_key_to_utc_timestamp, next_date_key, previous_date_key, timestamp_to_date_key,
        TimeError,
    },
};

use crate::{errors::CycleError, schema::Cycle, triggers::Readiness, ICycle};

/// Maximum number of completed UTC days that `derive_active_utc_day` scans backward
/// and catches up (one oldest unallocated day per hourly run, OIP-00343).
pub const MAX_REWARD_CATCHUP_DAYS: u32 = 64;

/// `ICycle.DayEmissionLimitAllocationFailed.reason`: the daily emission limit allocation step reverted.
pub const ALLOCATION_FAILED_STEP_REVERTED: u8 = 1;

fn latest_settled_day_through(
    storage: StorageHandle<'_>,
    from_day: u32,
    start_date: u32,
) -> Result<Option<u32>> {
    let mut d = from_day;
    loop {
        if let Some(record) = outbe_emission::api::get_emission_day_record(storage.clone(), d)? {
            if record.state == emission_day_state::PROCESSING
                || record.state == emission_day_state::COMPLETED
            {
                return Ok(Some(d));
            }
        }
        if d <= start_date {
            return Ok(None);
        }
        d = previous_date_key(d).map_err(calendar_step_error)?;
    }
}

/// Derives the active UTC calendar day without a stored cursor (OIP-00343).
///
/// Scans backward from `yesterday = previous_date_key(block_utc_day)` down to
/// `max(start_date, yesterday - 63)` (`MAX_REWARD_CATCHUP_DAYS = 64`), stopping at the first
/// `EmissionDay` whose `state` is `PROCESSING` or `COMPLETED` (pre-linked future
/// shells have `state == PENDING` and do not stop the scan).
pub fn derive_active_utc_day(
    storage: StorageHandle<'_>,
    genesis_timestamp: u64,
    block_timestamp: u64,
) -> Result<u32> {
    if genesis_timestamp == 0 {
        return Ok(0);
    }
    let start_date = timestamp_to_date_key(genesis_timestamp);
    if !is_valid_utc_day(start_date) {
        return Ok(start_date);
    }
    let block_utc_day = timestamp_to_date_key(block_timestamp);
    if !is_valid_utc_day(block_utc_day) || block_utc_day <= start_date {
        return Ok(start_date);
    }
    let yesterday = previous_date_key(block_utc_day).map_err(calendar_step_error)?;
    let yesterday_ts = date_key_to_utc_timestamp(yesterday).map_err(calendar_step_error)?;
    let catchup_span_secs = u64::from(MAX_REWARD_CATCHUP_DAYS.saturating_sub(1))
        * outbe_primitives::time::SECONDS_PER_DAY;
    let catchup_floor = timestamp_to_date_key(yesterday_ts.saturating_sub(catchup_span_secs));
    let floor_day = start_date.max(catchup_floor);
    let d_settled = latest_settled_day_through(storage, yesterday, floor_day)?;
    match d_settled {
        Some(settled) => next_date_key(settled).map_err(calendar_step_error),
        None => Ok(floor_day),
    }
}

/// Calendar work owned by one hourly ProtocolCycle execution.
///
/// This type is intentionally not persisted and not exposed through the EVM ABI.
/// The executor shares it, so the executor derives command authority from exactly
/// the same calendar decision as the handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolDayAction {
    SameDay,
    SettlePrevious { day: u32 },
}

/// Selects the daily action for an hourly ProtocolCycle execution.
///
/// If `active_utc_day < block_utc_day`, settles the single oldest unallocated UTC day
/// `active_utc_day` (`E_next`), catching up at most one day per hourly run within `MAX_REWARD_CATCHUP_DAYS`.
pub fn protocol_day_action(active_utc_day: u32, block_utc_day: u32) -> Result<ProtocolDayAction> {
    if !is_valid_utc_day(active_utc_day) {
        return Err(CycleError::InvalidActiveUtcDay {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }
    if !is_valid_utc_day(block_utc_day) {
        return Err(CycleError::InvalidBlockUtcDay {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }
    if active_utc_day > block_utc_day {
        return Err(CycleError::ActiveUtcDayAfterBlock {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }

    if active_utc_day == block_utc_day {
        return Ok(ProtocolDayAction::SameDay);
    }
    Ok(ProtocolDayAction::SettlePrevious {
        day: active_utc_day,
    })
}

/// Maps a date step to the cycle error class.
///
/// `TimeError` is `non_exhaustive`. The trailing arm keeps `PreGenesis` and any
/// later variant on the fatal path. It does not invent a `DateStep` for them.
fn calendar_step_error(err: TimeError) -> PrecompileError {
    match err {
        TimeError::DateBeforeMinimum { date_key } => {
            CycleError::DateBeforeMinimum { date_key }.into()
        }
        TimeError::DateOverflow { date_key } => CycleError::DateOverflow { date_key }.into(),
        TimeError::TimestampStepOverflow { date_key } => {
            CycleError::TimestampStepOverflow { date_key }.into()
        }
        err => PrecompileError::Fatal(err.to_string()),
    }
}

/// Returns `true` when `date_key` is a `YYYYMMDD` UTC day from 1970 or later
/// that round-trips through its UTC midnight timestamp.
fn is_valid_utc_day(date_key: u32) -> bool {
    let year = date_key / 10_000;
    let month = (date_key / 100) % 100;
    let day = date_key % 100;
    year >= 1970
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && date_key_to_utc_timestamp(date_key).is_ok_and(|ts| timestamp_to_date_key(ts) == date_key)
}

fn gas(ctx: &BlockRuntimeContext) -> u64 {
    ctx.storage.gas_used().unwrap_or(0)
}

fn gas_sample_delta(later: u64, earlier: u64) -> u64 {
    later.saturating_sub(earlier)
}

fn wrap(step: &str, r: Result<()>) -> Result<()> {
    r.map_err(|e| {
        tracing::error!(target: "outbe::cycle", step, error = ?e, "emission_limit_daily step failed");
        e
    })
}

/// Settles one explicitly owned, completed UTC day inside its own checkpoint.
/// Calendar selection belongs to [`run_protocol_cycle`].
pub fn settle_emission_day(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<()> {
    if day_settlement(ctx, prev_day)? == DaySettlement::Settled {
        return Ok(());
    }
    ctx.storage
        .with_checkpoint(|| settle_emission_day_inner(ctx, prev_day))
}

fn settle_emission_day_inner(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<()> {
    trace_settlement_dates(ctx, prev_day);

    let g0 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", gas_used = g0, prev_day, block_ts = ctx.block.timestamp, "entry");

    let day_number = day_number(ctx, prev_day)?;
    let cap = emission_limit_of_day(day_number);
    if cap.is_zero() {
        return Ok(());
    }
    let shares = EmissionShares::allocate(cap)?;

    let g1 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = gas_sample_delta(g1, g0), cumulative = g1, "after allocate_day_emission_limit");

    wrap(
        "seal_votes_and_fees",
        outbe_rewards::api::seal_votes_and_fees(ctx, prev_day),
    )?;
    let validator =
        outbe_rewards::api::allocate_day_validator_rewards(ctx, prev_day, shares.validator)?;
    let validator_topup = validator.topup;
    let fee_covered = validator.fee_covered;

    let g2 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = gas_sample_delta(g2, g1), cumulative = g2, "after validator top-up");

    let dist = allocate_day_agent_rewards(
        ctx,
        prev_day.into(),
        DayAgentPools {
            validator_topup,
            waa: shares.waa,
            sra: shares.sra,
            cca: shares.cca,
        },
    )?;
    let pool_excess = dist.total_excess;

    let g3 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = gas_sample_delta(g3, g2), cumulative = g3, "after reward distribute");

    let metadosis_total = shares.terminal_total(fee_covered, pool_excess)?;
    wrap(
        "record_day_allocation",
        outbe_emission::api::record_day_allocation(
            ctx.storage.clone(),
            prev_day,
            DayAllocationAmounts {
                emission_limit: cap,
                metadosis_limit: metadosis_total,
                validators: dist.validators_allocated,
                waa: dist.waa_allocated,
                sra: dist.sra_allocated,
                cca: dist.cca_allocated,
                promis_limit: U256::ZERO,
            },
        )
        .map(|_| ()),
    )?;

    dispatch_terminal_remainder(ctx, prev_day, metadosis_total)?;

    let g4 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = gas_sample_delta(g4, g3), cumulative = g4, "after terminal dispatch");

    let g5 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = gas_sample_delta(g5, g4), cumulative = g5, total = gas_sample_delta(g5, g0), "completed");

    tracing::info!(
        target: "outbe::cycle",
        prev_day,
        day_number,
        cap = %cap,
        validator_amount = %shares.validator,
        validator_topup = %validator_topup,
        pool_excess = %pool_excess,
        metadosis_total = %metadosis_total,
        total_gas = gas_sample_delta(g5, g0),
        "emission_limit_daily handler completed"
    );

    Ok(())
}

/// Settlement state of one completed UTC day before its economic calculation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaySettlement {
    /// The day has not yet been allocated in `outbe-emission`.
    Pending,
    /// The day has already been allocated (`state == PROCESSING || state == COMPLETED`).
    Settled,
}

/// Reads the `EmissionDay` state of `prev_day`.
fn day_settlement(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<DaySettlement> {
    let settled = outbe_emission::api::get_emission_day_record(ctx.storage.clone(), prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "get_emission_day_record", prev_day, error = ?e, "emission_limit_daily step failed");
            e
        })?
        .is_some_and(|rec| {
            rec.state == emission_day_state::PROCESSING
                || rec.state == emission_day_state::COMPLETED
        });
    if settled {
        tracing::debug!(
            target: "outbe::cycle",
            prev_day,
            block_number = ctx.block.block_number,
            "emission_limit_daily: prev_day already settled - skipping (idempotent)"
        );
        return Ok(DaySettlement::Settled);
    }
    Ok(DaySettlement::Pending)
}

/// Logs the calendar inputs of one settlement. The genesis UTC day read stays
/// before the first gas sample of the settlement.
fn trace_settlement_dates(ctx: &BlockRuntimeContext, prev_day: u32) {
    let block_ts = ctx.block.timestamp;
    let current_day = timestamp_to_date_key(block_ts);
    let start_date = timestamp_to_date_key(ctx.block.genesis_timestamp);
    tracing::info!(
        target: "outbe::cycle",
        block_ts,
        current_day,
        prev_day,
        start_date,
        block_number = ctx.block.block_number,
        "emission_limit_daily dates"
    );
}

/// Returns the number of days from genesis to `prev_day`.
fn day_number(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<u32> {
    outbe_rewards::runtime::day_number_since_genesis(ctx, prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "day_number_since_genesis", prev_day, error = ?e, "emission_limit_daily step failed");
            e
        })
}

/// Daily emission cap split into its sink amounts.
struct EmissionShares {
    validator: U256,
    waa: U256,
    sra: U256,
    cca: U256,
    metadosis: U256,
}

impl EmissionShares {
    /// Splits `cap` with the EmissionLimit allocation table. A sink without an
    /// allocation row gets zero.
    fn allocate(cap: U256) -> Result<Self> {
        let allocations = allocate_day_emission_limit(cap)?;
        let amount_for = |id: EmissionSinkId| -> U256 {
            allocations
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.amount)
                .unwrap_or(U256::ZERO)
        };
        Ok(Self {
            validator: amount_for(EmissionSinkId::Validator),
            waa: amount_for(EmissionSinkId::Waa),
            sra: amount_for(EmissionSinkId::Sra),
            cca: amount_for(EmissionSinkId::Cca),
            metadosis: amount_for(EmissionSinkId::Metadosis),
        })
    }

    /// Returns the terminal Metadosis amount: the Metadosis share plus the
    /// fee-covered validator share and the undistributed pool excess.
    fn terminal_total(&self, fee_covered: U256, pool_excess: U256) -> Result<U256> {
        self.metadosis
            .checked_add(fee_covered)
            .and_then(|v| v.checked_add(pool_excess))
            .ok_or_else(|| CycleError::MetadosisTerminalOverflow.into())
    }
}

/// Sends the terminal Metadosis remainder of `prev_day`. Then checks that
/// Metadosis formed the day limit for `prev_day` with the same base limit.
fn dispatch_terminal_remainder(
    ctx: &BlockRuntimeContext,
    prev_day: u32,
    metadosis_total: U256,
) -> Result<()> {
    let prev_day_ts = date_key_to_utc_timestamp(prev_day).map_err(calendar_step_error)?;
    wrap(
        "set_metadosis_limit",
        set_metadosis_limit(ctx, metadosis_total, prev_day_ts),
    )?;
    let emission_day = outbe_emission::api::get_emission_day_record(ctx.storage.clone(), prev_day)?
        .ok_or(CycleError::AllocationWithoutMetadosisReceipt)?;
    if emission_day.day == prev_day && U256::from(emission_day.metadosis_limit) == metadosis_total {
        Ok(())
    } else {
        Err(CycleError::AllocationDisagreesWithMetadosisReceipt.into())
    }
}

/// Processes the shared four-pool accrual budget and bounded owner cleanup.
pub(crate) fn credit_reward_slice(ctx: &BlockRuntimeContext) -> Result<()> {
    process_agent_rewards(ctx).map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "process_agent_rewards", error = ?e, "protocol cycle step failed");
        e
    })
}

fn is_recoverable_step_error(err: &PrecompileError) -> bool {
    err.record_pass_failure() == outbe_primitives::error::PassFailure::Skip
}

pub(crate) fn run_advance_worldwide_days_step(
    ctx: &BlockRuntimeContext,
    block_utc_day: u32,
) -> Result<()> {
    match ctx
        .storage
        .with_checkpoint(|| outbe_tributefactory::worldwideday::advance_worldwide_days_command(ctx))
    {
        Ok(()) => Ok(()),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                block_utc_day,
                error = ?err,
                "advance_worldwide_days step reverted; inner checkpoint rolled back"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Allocates `day` and returns it when Metadosis of `day` is to run in this run.
fn run_allocate_emission_day_step(ctx: &BlockRuntimeContext, day: u32) -> Result<Option<u32>> {
    if day_settlement(ctx, day)? == DaySettlement::Settled {
        return Ok(None);
    }
    match settle_emission_day(ctx, day) {
        Ok(()) => Ok(Some(day)),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                day,
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                error = ?err,
                "settle_emission_day step reverted; routing day emission limit to PromisLimit"
            );
            record_failed_allocation_fallback(ctx, day).map(|()| None)
        }
        Err(err) => Err(err),
    }
}

fn record_failed_allocation_fallback(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    ctx.storage.with_checkpoint(|| {
        let day_num = day_number(ctx, day)?;
        let cap = emission_limit_of_day(day_num);
        if day > outbe_rewards::api::get_last_sealed_day(&ctx.storage)? {
            outbe_rewards::api::seal_votes_and_fees(ctx, day)?;
        }
        outbe_agentreward::api::abandon_day_agent_rewards(ctx, day.into())?;
        if !cap.is_zero() {
            outbe_promislimit::PromisLimitContract::new(ctx.storage.clone()).checked_credit(cap)?;
        }
        outbe_emission::api::record_failed_day_allocation(ctx.storage.clone(), day, cap)?;
        ctx.storage
            .contract::<Cycle<'_>>()
            .emit(ICycle::DayEmissionLimitAllocationFailed {
                emissionDay: day,
                emissionLimitMinor: cap,
                reason: ALLOCATION_FAILED_STEP_REVERTED,
            })?;
        outbe_metadosis::commands::emit_canceled_after_failed_allocation(ctx, day)
    })
}

fn run_metadosis_step(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    match ctx
        .storage
        .with_checkpoint(|| outbe_metadosis::commands::process_emission_day(ctx, day))
    {
        Ok(()) => Ok(()),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                error = ?err,
                day,
                "process_emission_day step reverted; routing metadosis limit to PromisLimit"
            );
            record_failed_metadosis_fallback(ctx, day)
        }
        Err(err) => Err(err),
    }
}

fn record_failed_metadosis_fallback(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    ctx.storage
        .with_checkpoint(|| outbe_metadosis::commands::fail_metadosis(ctx, day))
}

/// Runs the single hourly protocol orchestration entry point (`OIP-00343`). Until the votes of
/// the day to allocate are sealed, it changes nothing and answers not ready.
/// 1. `settle_emission_day` (step A4) for the oldest unallocated day `E_next` in its own
///    checkpoint with fallback to `record_failed_day_allocation` + `PromisLimit`.
/// 2. `advance_worldwide_days` (step A6) once per UTC day at 00:00 UTC in its own checkpoint,
///    logging the cause and retrying on the next hour if it reverts.
/// 3. `process_emission_day` (step A7) for the day that step A4 allocated, in its own checkpoint
///    with fallback to `record_metadosis_failed` + `PromisLimit`.
pub fn run_protocol_cycle(ctx: &BlockRuntimeContext) -> Result<Readiness> {
    run_protocol_cycle_steps(ctx).map_err(fail_block)
}

/// A revert that reaches the run comes from a step fallback or from code outside the step
/// checkpoints. The runner would consume the slot and try the same day again, so it fails the block.
fn fail_block(err: PrecompileError) -> PrecompileError {
    match err {
        err @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_)) => {
            PrecompileError::FatalWithCause(Box::new(err))
        }
        err => err,
    }
}

fn run_protocol_cycle_steps(ctx: &BlockRuntimeContext) -> Result<Readiness> {
    let block_utc_day = timestamp_to_date_key(ctx.block.timestamp);
    let active_utc_day = derive_active_utc_day(
        ctx.storage.clone(),
        ctx.block.genesis_timestamp,
        ctx.block.timestamp,
    )?;
    let day_action = protocol_day_action(active_utc_day, block_utc_day)?;

    let allocated = match day_action {
        ProtocolDayAction::SameDay => None,
        ProtocolDayAction::SettlePrevious { day } => {
            if !outbe_rewards::api::is_voting_complete(ctx, day)? {
                return Ok(Readiness::NotReady);
            }
            run_allocate_emission_day_step(ctx, day)?
        }
    };

    run_advance_worldwide_days_step(ctx, block_utc_day)?;

    if let Some(day) = allocated {
        run_metadosis_step(ctx, day)?;
    }

    tracing::info!(
        target: "outbe::cycle",
        block_number = ctx.block.block_number,
        block_timestamp = ctx.block.timestamp,
        active_utc_day,
        block_utc_day,
        day_action = ?day_action,
        "protocol cycle completed"
    );
    Ok(Readiness::Done)
}

#[cfg(test)]
pub(crate) fn run_emission_limit_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    let day = outbe_primitives::time::previous_date_key(timestamp_to_date_key(ctx.block.timestamp))
        .expect("date step");
    settle_emission_day(ctx, day)?;
    outbe_metadosis::commands::process_emission_day(ctx, day)
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    #[test]
    fn a_decreasing_gas_sample_reports_zero_and_an_increase_keeps_the_difference() {
        assert_eq!(gas_sample_delta(3, 10), 0);
        assert_eq!(gas_sample_delta(10, 3), 7);
    }
}
