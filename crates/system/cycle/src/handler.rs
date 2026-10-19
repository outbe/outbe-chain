//! ProtocolCycle calendar orchestration and completed-day emission settlement.
//!
//! This is the natural home of the `Cycle -> EmissionLimit -> AgentReward
//! -> Rewards -> Metadosis` orchestration. The orchestration lives in `outbe-cycle`
//! (rather than `outbe-emissionlimit`) to avoid a `outbe-emissionlimit -> outbe-rewards`
//! dependency edge. That edge would close the cycle with the existing
//! `outbe-rewards -> outbe-emissionlimit` re-export.

use alloy_primitives::U256;

use outbe_agentreward::distribution::{advance_accruals, distribute_daily, PoolKind};
use outbe_emissionlimit::{
    allocation::{allocate_emission, EmissionSinkId},
    block::{dispatch_late_settlement_residue_at, dispatch_terminal_remainder_at},
    day_emission::day_emission_limit,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    time::{date_key_to_utc_timestamp, next_date_key, timestamp_to_date_key},
    units::{checked_protocol_to_native, native_to_protocol_floor},
};

use crate::errors::CycleError;

/// Calendar work owned by one hourly ProtocolCycle execution.
///
/// This type is intentionally not persisted and not exposed through the EVM ABI.
/// The executor shares it, so the executor derives command authority from exactly
/// the same calendar decision as the handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolDayAction {
    SameDay,
    SettlePrevious { day: u32 },
    SkipMissed { from: u32, to: u32 },
}

/// Selects the daily action for an hourly ProtocolCycle execution.
///
/// Only a contiguous UTC-day transition is settled. If the chain advances by
/// more than one calendar day, every completed day in that gap is deliberately
/// forfeited: no economic state is synthesized after the halt.
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
    if next_date_key(active_utc_day) == block_utc_day {
        return Ok(ProtocolDayAction::SettlePrevious {
            day: active_utc_day,
        });
    }
    Ok(ProtocolDayAction::SkipMissed {
        from: active_utc_day,
        to: block_utc_day,
    })
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
        && timestamp_to_date_key(date_key_to_utc_timestamp(date_key)) == date_key
}

fn gas(ctx: &BlockRuntimeContext) -> u64 {
    ctx.storage.gas_used().unwrap_or(0)
}

/// Applies raw 18-decimal gas fees to a six-decimal validator emission budget,
/// then floors only the remaining native amount back to protocol precision.
fn validator_topup_protocol(validator_amount: U256, fees_native: U256) -> Result<U256> {
    let native_budget = checked_protocol_to_native(validator_amount)
        .ok_or(CycleError::NativeValidatorBudgetOverflow)?;
    Ok(native_to_protocol_floor(
        native_budget.saturating_sub(fees_native),
    ))
}

fn wrap(step: &str, r: Result<()>) -> Result<()> {
    r.map_err(|e| {
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!(target: "outbe::cycle", step, error = ?e, backtrace = %bt, "emission_limit_daily step failed");
        e
    })
}

/// Settles one explicitly owned, completed UTC day. Calendar selection belongs
/// to [`run_protocol_cycle`]. This function only performs the existing daily
/// economic calculation and commits its idempotency pair.
pub fn settle_emission_day(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<()> {
    if day_settlement(ctx, prev_day)? == DaySettlement::Settled {
        return Ok(());
    }
    trace_settlement_dates(ctx, prev_day);

    let g0 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", gas_used = g0, prev_day, block_ts = ctx.block.timestamp, "entry");

    let day_number = day_number(ctx, prev_day)?;
    let cap = day_emission_limit(day_number);
    if cap.is_zero() {
        return Ok(());
    }
    let shares = EmissionShares::allocate(cap)?;

    let g1 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g1 - g0, cumulative = g1, "after allocate_emission");

    let validator_topup = validator_topup(ctx, prev_day, shares.validator)?;
    wrap(
        "close_day_participation",
        outbe_rewards::api::close_day_participation(ctx, prev_day),
    )?;
    let fee_covered = shares
        .validator
        .checked_sub(validator_topup)
        .ok_or(CycleError::ValidatorTopupExceedsAllocation)?;

    let g2 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g2 - g1, cumulative = g2, "after validator top-up");

    let pool_excess = distribute_reward_pools(ctx, prev_day, &shares, validator_topup)?;

    let g3 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g3 - g2, cumulative = g3, "after reward distribute");

    let metadosis_total = shares.terminal_total(fee_covered, pool_excess)?;
    dispatch_terminal_remainder(ctx, prev_day, metadosis_total)?;

    let g4 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g4 - g3, cumulative = g4, "after terminal dispatch");

    wrap(
        "mark_day_settled",
        outbe_rewards::api::mark_day_settled(ctx, prev_day),
    )?;

    let g5 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g5 - g4, cumulative = g5, total = g5 - g0, "completed");

    tracing::info!(
        target: "outbe::cycle",
        prev_day,
        day_number,
        cap = %cap,
        validator_amount = %shares.validator,
        validator_topup = %validator_topup,
        pool_excess = %pool_excess,
        metadosis_total = %metadosis_total,
        total_gas = g5 - g0,
        "emission_limit_daily handler completed"
    );

    Ok(())
}

/// Settlement state of one completed UTC day before its economic calculation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaySettlement {
    /// The day has no settled marker and no Metadosis day-limit receipt.
    Pending,
    /// The day has both its settled marker and its Metadosis receipt.
    Settled,
}

/// Checks the settlement preconditions and the idempotency pair of `prev_day`.
///
/// The reward participation windows of `prev_day` must be closed. The Cycle
/// `daily_settled` marker and the Metadosis day-limit receipt must agree.
fn day_settlement(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<DaySettlement> {
    if !outbe_rewards::api::day_participation_complete(ctx, prev_day)? {
        return Err(CycleError::SettlementBeforeParticipationWindowsClose.into());
    }

    // Idempotency guard. This handler credits the reward pools and sends
    // terminal Metadosis with no per-credit day guard. A second call for the
    // same `prev_day` would credit those pools again. `run_protocol_cycle` advances
    // `active_utc_day` in the same checkpoint as this settlement. A later
    // CycleTick does not select that `prev_day` again. `daily_settled[prev_day]`
    // remains the defensive guard. Each day still settles once.
    let settled = outbe_rewards::api::is_day_settled(ctx, prev_day).map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "is_day_settled", prev_day, error = ?e, "emission_limit_daily step failed");
        e
    })?;
    let existing_formation =
        outbe_metadosis::api::day_limit_formation_receipt(ctx.storage.clone(), prev_day.into())?;
    match (settled, existing_formation.is_some()) {
        (true, false) => Err(CycleError::DailySettledWithoutMetadosisReceipt.into()),
        (false, true) => Err(CycleError::MetadosisReceiptWithoutDailySettled.into()),
        (true, true) => {
            tracing::debug!(
                target: "outbe::cycle",
                prev_day,
                block_number = ctx.block.block_number,
                "emission_limit_daily: prev_day already settled - skipping (idempotent)"
            );
            Ok(DaySettlement::Settled)
        }
        (false, false) => Ok(DaySettlement::Pending),
    }
}

/// Logs the calendar inputs of one settlement. The genesis UTC day read stays
/// before the first gas sample of the settlement.
fn trace_settlement_dates(ctx: &BlockRuntimeContext, prev_day: u32) {
    let block_ts = ctx.block.timestamp;
    let current_day = timestamp_to_date_key(block_ts);
    let genesis = outbe_rewards::runtime::genesis_utc_day(ctx).unwrap_or(0);
    tracing::info!(
        target: "outbe::cycle",
        block_ts,
        current_day,
        prev_day,
        genesis_utc_day = genesis,
        block_number = ctx.block.block_number,
        "emission_limit_daily dates"
    );
}

/// Returns the number of days from genesis to `prev_day`.
fn day_number(ctx: &BlockRuntimeContext, prev_day: u32) -> Result<u32> {
    outbe_rewards::runtime::day_number_since_genesis(ctx, prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "day_number_since_genesis", prev_day, error = ?e, "emission_limit_daily step failed");
            let _bt = std::backtrace::Backtrace::force_capture();
            tracing::error!(target: "outbe::cycle", backtrace = %_bt, "stacktrace");
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
        let allocations = allocate_emission(cap)?;
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

/// The validator top-up of `prev_day`: the validator share minus the day's raw
/// fees, in protocol units. AgentReward credits it to the validator pool.
fn validator_topup(
    ctx: &BlockRuntimeContext,
    prev_day: u32,
    validator_amount: U256,
) -> Result<U256> {
    if validator_amount.is_zero() {
        return Ok(U256::ZERO);
    }
    let fees = outbe_rewards::api::read_daily_fee_sum_raw(ctx, prev_day).map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "read_daily_fee_sum_raw", error = ?e, "emission_limit_daily step failed");
        e
    })?;
    validator_topup_protocol(validator_amount, fees)
}

/// Distributes the WAA, SRA, CCA and validator pools of `prev_day`. Returns the
/// undistributed excess.
fn distribute_reward_pools(
    ctx: &BlockRuntimeContext,
    prev_day: u32,
    shares: &EmissionShares,
    validator_topup: U256,
) -> Result<U256> {
    distribute_daily(
        ctx,
        prev_day.into(),
        &[
            (PoolKind::Waa, shares.waa),
            (PoolKind::Sra, shares.sra),
            (PoolKind::Cca, shares.cca),
            (PoolKind::Validator, validator_topup),
        ],
    )
    .map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "distribute_daily", error = ?e, "emission_limit_daily step failed");
        e
    })
}

/// Sends the terminal Metadosis remainder of `prev_day`. Then checks that
/// Metadosis formed the day limit for `prev_day` with the same base limit.
fn dispatch_terminal_remainder(
    ctx: &BlockRuntimeContext,
    prev_day: u32,
    metadosis_total: U256,
) -> Result<()> {
    let prev_day_ts = date_key_to_utc_timestamp(prev_day);
    wrap(
        "dispatch_terminal_remainder_at",
        dispatch_terminal_remainder_at(ctx, metadosis_total, prev_day_ts),
    )?;
    let formation =
        outbe_metadosis::api::day_limit_formation_receipt(ctx.storage.clone(), prev_day.into())?
            .ok_or(CycleError::AllocationWithoutMetadosisReceipt)?;
    match formation {
        outbe_metadosis::DayLimitFormationReceipt::Formed(formed)
            if formed.worldwide_day.value() == prev_day && formed.base_limit == metadosis_total =>
        {
            Ok(())
        }
        outbe_metadosis::DayLimitFormationReceipt::Formed(_) => {
            Err(CycleError::AllocationDisagreesWithMetadosisReceipt.into())
        }
    }
}

/// Credits this block's slice of the queued WAA and SRA day pools and sends the
/// rounding residue of the finished ones to the Promis Limit carry-over.
fn credit_reward_slice(ctx: &BlockRuntimeContext) -> Result<()> {
    let residue = advance_accruals(ctx).map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "advance_accruals", error = ?e, "protocol cycle step failed");
        e
    })?;
    wrap(
        "dispatch_late_settlement_residue_at",
        dispatch_late_settlement_residue_at(ctx, residue, ctx.block.timestamp),
    )
}

/// Runs the single hourly protocol orchestration entry point.
///
/// This function settles a contiguous completed UTC day before the existing
/// Metadosis command runs. It forfeits multi-day gaps and advances the calendar
/// cursor without synthesizing economic state. The existing command then runs
/// exactly once to:
/// - create the current WWD.
/// - advance every active WWD.
/// - process at most one READY WWD.
pub fn run_protocol_cycle(ctx: &BlockRuntimeContext) -> Result<()> {
    let block_utc_day = timestamp_to_date_key(ctx.block.timestamp);
    let cycle: crate::schema::Cycle<'_> = ctx.storage.contract::<crate::schema::Cycle<'_>>();
    let active_utc_day = cycle.active_utc_day.read()?;
    if active_utc_day == 0 {
        return Err(CycleError::ActiveUtcDayNotInitialized.into());
    }

    let day_action = protocol_day_action(active_utc_day, block_utc_day)?;
    match day_action {
        ProtocolDayAction::SameDay => {}
        ProtocolDayAction::SettlePrevious { day } => {
            settle_emission_day(ctx, day)?;
            cycle.active_utc_day.write(block_utc_day)?;
        }
        ProtocolDayAction::SkipMissed { from, to } => {
            cycle.active_utc_day.write(block_utc_day)?;
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                skipped_from_utc_day = from,
                current_utc_day = to,
                "protocol cycle forfeited missed UTC days after a multi-day chain halt"
            );
        }
    }

    credit_reward_slice(ctx)?;

    wrap(
        "start_metadosis",
        outbe_metadosis::commands::start_metadosis(ctx),
    )?;

    tracing::info!(
        target: "outbe::cycle",
        block_number = ctx.block.block_number,
        block_timestamp = ctx.block.timestamp,
        active_utc_day,
        block_utc_day,
        day_action = ?day_action,
        "protocol cycle completed"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) fn run_emission_limit_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    settle_emission_day(
        ctx,
        outbe_primitives::time::previous_date_key(timestamp_to_date_key(ctx.block.timestamp)),
    )?;
    outbe_metadosis::commands::start_metadosis(ctx)
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use outbe_primitives::units::NATIVE_UNITS_PER_PROTOCOL_UNIT;

    #[test]
    fn validator_fees_stay_native_until_the_remaining_budget_is_floored() {
        let allocation = U256::from(2u64);
        let one_protocol_unit_native = NATIVE_UNITS_PER_PROTOCOL_UNIT;

        assert_eq!(
            validator_topup_protocol(allocation, one_protocol_unit_native - U256::ONE).unwrap(),
            U256::ONE
        );
        assert_eq!(
            validator_topup_protocol(allocation, one_protocol_unit_native + U256::ONE).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            validator_topup_protocol(allocation, one_protocol_unit_native * U256::from(2u64))
                .unwrap(),
            U256::ZERO
        );
    }
}
