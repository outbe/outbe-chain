//! Daily emission orchestrator wired in as the
//! [`crate::triggers::TriggerId::EmissionLimit1`] handler.
//!
//! This is the natural home of the `Cycle → EmissionLimit → AgentReward
//! → Rewards` orchestration described in the epic. Putting
//! it inside `outbe-cycle` (rather than `outbe-emissionlimit`) avoids
//! a `outbe-emissionlimit → outbe-rewards` dependency edge, which
//! would close the cycle with the existing
//! `outbe-rewards → outbe-emissionlimit` re-export.

use alloy_primitives::U256;

use outbe_emissionlimit::{
    allocation::{allocate_emission, EmissionSinkId},
    block::dispatch_terminal_remainder_at,
    daily_emission::day_emission_limit,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    time::{date_key_to_timestamp, previous_date_key, timestamp_to_date_key},
};

fn gas(ctx: &BlockRuntimeContext) -> u64 {
    ctx.storage.gas_used().unwrap_or(0)
}

fn wrap(step: &str, r: Result<()>) -> Result<()> {
    r.map_err(|e| {
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!(target: "outbe::cycle", step, error = ?e, backtrace = %bt, "emission_limit_daily step failed");
        e
    })
}

pub fn run_emission_limit_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let current_day = timestamp_to_date_key(block_ts);
    let prev_day = previous_date_key(current_day);

    // idempotency guard. This handler mints the CCA/Merchant agent pools
    // and re-dispatches terminal Metadosis with no PER-MINT day guard (only the
    // validator topup is independently idempotent via `daily_topup_settled`), so
    // a second invocation for an already-settled `prev_day` would double-mint
    // those pools. That re-fire is reachable whenever more than one CycleTick
    // resolves the same `prev_day` (e.g. several blocks within one UTC day after
    // a forward timestamp advance — bounded but not eliminated by the C-01 drift
    // band). Gate the WHOLE settlement on `daily_settled[prev_day]` so each day
    // settles exactly once regardless of how many times the handler fires.
    if outbe_rewards::api::is_day_settled(ctx, prev_day).map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "is_day_settled", prev_day, error = ?e, "emission_limit_daily step failed");
        e
    })? {
        tracing::debug!(
            target: "outbe::cycle",
            prev_day,
            block_number = ctx.block.block_number,
            "emission_limit_daily: prev_day already settled — skipping (idempotent)"
        );
        return Ok(());
    }

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

    let g0 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", gas_used = g0, prev_day, block_ts, "entry");

    let day_number = outbe_rewards::runtime::day_number_since_genesis(ctx, prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "day_number_since_genesis", prev_day, error = ?e, "emission_limit_daily step failed");
            let _bt = std::backtrace::Backtrace::force_capture();
            tracing::error!(target: "outbe::cycle", backtrace = %_bt, "stacktrace");
            e
        })?;

    let cap = day_emission_limit(day_number);
    if cap.is_zero() {
        return Ok(());
    }

    let allocations = allocate_emission(cap)?;
    let amount_for = |id: EmissionSinkId| -> U256 {
        allocations
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.amount)
            .unwrap_or(U256::ZERO)
    };
    let validator_amount = amount_for(EmissionSinkId::Validator);
    let waa_amount = amount_for(EmissionSinkId::Waa);
    let sra_amount = amount_for(EmissionSinkId::Sra);
    let cca_amount = amount_for(EmissionSinkId::Cca);
    let merchant_amount = amount_for(EmissionSinkId::Merchant);
    let metadosis_amount = amount_for(EmissionSinkId::Metadosis);

    let g1 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g1 - g0, cumulative = g1, "after allocate_emission");

    let fees = outbe_rewards::api::read_daily_fee_sum_raw(ctx, prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "read_daily_fee_sum_raw", error = ?e, "emission_limit_daily step failed");
            let _bt = std::backtrace::Backtrace::force_capture();
            tracing::error!(target: "outbe::cycle", backtrace = %_bt, "stacktrace");
            e
        })?;
    let voters = outbe_rewards::api::read_voters_for_day(ctx, prev_day)
        .map_err(|e| {
            tracing::error!(target: "outbe::cycle", step = "read_voters_for_day", error = ?e, "emission_limit_daily step failed");
            let _bt = std::backtrace::Backtrace::force_capture();
            tracing::error!(target: "outbe::cycle", backtrace = %_bt, "stacktrace");
            e
        })?;
    let validator_excess = if validator_amount.is_zero()
        || voters.is_empty()
        || fees >= validator_amount
    {
        validator_amount
    } else {
        let topup = validator_amount
            .checked_sub(fees)
            .ok_or_else(|| PrecompileError::Revert("validator topup underflow".into()))?;
        outbe_rewards::api::add_topup_for_voters(ctx, prev_day, topup, &voters)
                .map_err(|e| {
                    tracing::error!(target: "outbe::cycle", step = "add_topup_for_voters", error = ?e, "emission_limit_daily step failed");
                    e
                })?;
        fees
    };

    let g2 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g2 - g1, cumulative = g2, voters = voters.len(), "after validator pool");

    use outbe_agentreward::distribution::{distribute_daily, PoolKind};
    let agent_excess = distribute_daily(
        ctx,
        prev_day.into(),
        &[
            (PoolKind::Waa, waa_amount),
            (PoolKind::Sra, sra_amount),
            (PoolKind::Cca, cca_amount),
            (PoolKind::Merchant, merchant_amount),
        ],
    )
    .map_err(|e| {
        tracing::error!(target: "outbe::cycle", step = "distribute_daily", error = ?e, "emission_limit_daily step failed");
        e
    })?;

    let g3 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g3 - g2, cumulative = g3, "after agent distribute");

    let metadosis_total = metadosis_amount
        .checked_add(validator_excess)
        .and_then(|v| v.checked_add(agent_excess))
        .ok_or_else(|| PrecompileError::Revert("metadosis terminal overflow".into()))?;
    let prev_day_ts = date_key_to_timestamp(prev_day);
    wrap(
        "dispatch_terminal_remainder_at",
        dispatch_terminal_remainder_at(ctx, metadosis_total, prev_day_ts),
    )?;

    let g4 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g4 - g3, cumulative = g4, "after terminal dispatch");

    wrap(
        "start_metadosis",
        outbe_metadosis::runtime::start_metadosis(ctx),
    )?;

    let g5 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g5 - g4, cumulative = g5, "after start_metadosis");

    wrap(
        "mark_day_settled",
        outbe_rewards::api::mark_day_settled(ctx, prev_day),
    )?;

    let g6 = gas(ctx);
    tracing::debug!(target: "outbe::cycle::gas", step_gas = g6 - g5, cumulative = g6, total = g6 - g0, "completed");

    tracing::info!(
        target: "outbe::cycle",
        prev_day,
        day_number,
        cap = %cap,
        validator_amount = %validator_amount,
        validator_excess = %validator_excess,
        agent_excess = %agent_excess,
        metadosis_total = %metadosis_total,
        total_gas = g6 - g0,
        "emission_limit_daily handler completed"
    );

    Ok(())
}
