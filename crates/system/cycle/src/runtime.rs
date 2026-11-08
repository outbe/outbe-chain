//! Per-block trigger dispatch loop.
//!
//! Runs from [`handle_cycle_tick`] on every
//! block during the begin-zone CycleTick phase. Iterates the genesis-resolved
//! trigger table
//! and fires any trigger whose next slot has been reached.

use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::{PrecompileError, Result},
    system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
};

use crate::errors::CycleError;
use crate::handler::{protocol_day_action, ProtocolDayAction};
use crate::schema::Cycle;
use crate::state::{accounting_gate_blocks, EvmAccountingProgress};
use crate::triggers::{active_triggers, last_fire_at, next_fire_at, TriggerId, TriggerSpec};
use crate::ICycle;

/// Protocol module for `SystemTxKind::CycleTick` (`OSC2`).
pub struct CycleModule;

impl ProtocolModule for CycleModule {
    const NAME: &'static str = "cycle_tick";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let remaining_gas = ctx
            .storage
            .metered_gas_remaining()?
            .ok_or(CycleError::InsufficientWorkCapacity)?;
        let capacity = CycleWorkCapacity {
            remaining_gas,
            gas_used_at_entry: ctx.storage.gas_used()?,
            reserve_after_cycle: 0,
        };
        handle_cycle_tick(ctx, GENESIS_BOOTSTRAP_BLOCK_NUMBER, capacity)
    }
}

/// Calibrated Cycle body overhead, excluding handlers and caller prefix work.
/// `crates/blockchain/evm/src/executor/tests/receipts_and_gas/calibration.rs` measures 18,890,500
/// gross gas for bootstrap, dispatch, reward slicing and populated cleanup.
/// Fourfold headroom, rounded up to 1,000,000 gas, yields this reserve.
pub const CYCLE_OVERHEAD_RESERVE_GAS: u64 = 76_000_000;

#[cfg(feature = "test-utils")]
std::thread_local! {
    static TRIGGER_GAS_SAMPLES: std::cell::RefCell<Vec<(u32, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    static CYCLE_GAS_SAMPLES: std::cell::RefCell<Vec<(u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Drains gross handler-only gas samples on this test thread. Failed handlers
/// retain spent gas, so samples include ordinary reverts as well as successes.
#[cfg(feature = "test-utils")]
pub fn take_trigger_gas_samples() -> Vec<(u32, u64)> {
    TRIGGER_GAS_SAMPLES.with(|samples| core::mem::take(&mut *samples.borrow_mut()))
}

/// Drains successful Cycle body samples `(gross_body_gas, gross_handler_gas)`.
/// Body gas includes bootstrap, dispatch, reward slicing and auction cleanup, but excludes work
/// performed by the caller before entering Cycle. Subtraction gives overhead.
#[cfg(feature = "test-utils")]
pub fn take_cycle_gas_samples() -> Vec<(u64, u64)> {
    CYCLE_GAS_SAMPLES.with(|samples| core::mem::take(&mut *samples.borrow_mut()))
}

#[cfg(feature = "test-utils")]
fn record_cycle_gas_sample(ctx: &BlockRuntimeContext, before: u64, handler_gas: u64) -> Result<()> {
    let gross = ctx
        .storage
        .gas_used()?
        .checked_sub(before)
        .filter(|gross| *gross >= handler_gas)
        .ok_or_else(|| {
            PrecompileError::Fatal("Cycle observation gross gas counter regressed".into())
        })?;
    CYCLE_GAS_SAMPLES.with(|samples| samples.borrow_mut().push((gross, handler_gas)));
    Ok(())
}

/// Runs the CycleTick system-transaction body after parent accounting and
/// canonical late-credit windows. Fresh genesis initializes WorldwideDays at
/// block 1; other activation profiles supply their initialization height.
pub fn handle_cycle_tick(
    ctx: &BlockRuntimeContext<'_>,
    metadosis_genesis_activation_height: u64,
    capacity: CycleWorkCapacity,
) -> Result<()> {
    handle_cycle_tick_with_budgets(
        ctx,
        metadosis_genesis_activation_height,
        &active_triggers()?,
        capacity,
        CYCLE_OVERHEAD_RESERVE_GAS,
    )
}

/// Explicit budgeted Cycle body, including bootstrap, reward slicing and bounded
/// auction cleanup. The overhead must also cover all dispatcher bookkeeping.
pub fn handle_cycle_tick_with_budgets(
    ctx: &BlockRuntimeContext<'_>,
    metadosis_genesis_activation_height: u64,
    triggers: &[TriggerSpec],
    capacity: CycleWorkCapacity,
    overhead_reserve_gas: u64,
) -> Result<()> {
    #[cfg(feature = "test-utils")]
    let observation_before = ctx.storage.gas_used()?;
    let mut reserve = CycleWorkReserve::new(capacity, overhead_reserve_gas);
    reserve.available(ctx)?;
    if ctx.block.block_number == metadosis_genesis_activation_height {
        outbe_tributefactory::worldwideday::bootstrap_genesis_worldwide_days_command(ctx)?;
    }
    dispatch_budgeted(ctx, triggers, &mut reserve)?;
    crate::handler::credit_reward_slice(ctx)?;
    outbe_desis::process_cleanup(ctx.storage.clone())?;
    reserve.available(ctx)?;
    #[cfg(feature = "test-utils")]
    record_cycle_gas_sample(ctx, observation_before, reserve.handler_gas)?;
    Ok(())
}

/// Dispatches every active trigger whose `next_fire_at` is `<=
/// ctx.block.timestamp`. Each handler runs in an atomic work-budget scope.
/// Ordinary reverts consume the slot; budget exhaustion rolls back the handler
/// and leaves it and its due dependents pending. Independent work can continue.
/// Capacity deferral does not attempt the handler. Other errors reject CycleTick.
///
/// The typical case is a slow-running chain that produces blocks every few
/// seconds. In that case the dispatcher is a near-noop on every block. It
/// fires ProtocolCycle only on the first block whose timestamp crosses
/// the next UTC-hour boundary.
pub fn dispatch_triggers(ctx: &BlockRuntimeContext, capacity: CycleWorkCapacity) -> Result<()> {
    dispatch_triggers_with_budgets(
        ctx,
        &active_triggers()?,
        capacity,
        CYCLE_OVERHEAD_RESERVE_GAS,
    )
}

/// Aggregate gross capacity measured at `gas_used_at_entry`, before Cycle work.
/// The executor supplies the later-phase reserve; Cycle never reads EVM globals.
#[derive(Clone, Copy, Debug)]
pub struct CycleWorkCapacity {
    pub remaining_gas: u64,
    pub gas_used_at_entry: u64,
    pub reserve_after_cycle: u64,
}

impl CycleWorkCapacity {
    fn available(self, ctx: &BlockRuntimeContext, overhead_reserve_gas: u64) -> Result<u64> {
        let invalid = || PrecompileError::from(CycleError::InsufficientWorkCapacity);
        let spent = ctx
            .storage
            .gas_used()?
            .checked_sub(self.gas_used_at_entry)
            .ok_or_else(invalid)?;
        let aggregate = self
            .remaining_gas
            .checked_sub(spent)
            .and_then(|remaining| remaining.checked_sub(self.reserve_after_cycle))
            .and_then(|remaining| remaining.checked_sub(overhead_reserve_gas))
            .ok_or_else(invalid)?;
        let enclosing = ctx
            .storage
            .metered_gas_remaining()?
            .ok_or_else(invalid)?
            .checked_sub(overhead_reserve_gas)
            .ok_or_else(invalid)?;
        Ok(aggregate.min(enclosing))
    }
}

struct CycleWorkReserve {
    capacity: CycleWorkCapacity,
    overhead_gas: u64,
    handler_gas: u64,
}

impl CycleWorkReserve {
    fn new(capacity: CycleWorkCapacity, overhead_gas: u64) -> Self {
        Self {
            capacity,
            overhead_gas,
            handler_gas: 0,
        }
    }

    fn available(&self, ctx: &BlockRuntimeContext) -> Result<u64> {
        let invalid = || PrecompileError::from(CycleError::InsufficientWorkCapacity);
        let overhead_spent = ctx
            .storage
            .gas_used()?
            .checked_sub(self.capacity.gas_used_at_entry)
            .and_then(|spent| spent.checked_sub(self.handler_gas))
            .ok_or_else(invalid)?;
        let remaining_overhead = self
            .overhead_gas
            .checked_sub(overhead_spent)
            .ok_or_else(invalid)?;
        self.capacity.available(ctx, remaining_overhead)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkOutcome {
    NotRun,
    Executed,
    ConsumedFailure,
    Pending,
}

/// Explicit scheduler for a supplied trigger table. Callers must supply allowances and
/// a reserve that bounds the remaining dispatcher bookkeeping and reward slice.
pub fn dispatch_triggers_with_budgets(
    ctx: &BlockRuntimeContext,
    triggers: &[TriggerSpec],
    capacity: CycleWorkCapacity,
    overhead_reserve_gas: u64,
) -> Result<()> {
    let mut reserve = CycleWorkReserve::new(capacity, overhead_reserve_gas);
    dispatch_budgeted(ctx, triggers, &mut reserve)
}

fn dispatch_budgeted(
    ctx: &BlockRuntimeContext,
    triggers: &[TriggerSpec],
    reserve: &mut CycleWorkReserve,
) -> Result<()> {
    let mut outcomes = [WalkOutcome::NotRun; 9];
    reserve.available(ctx)?;
    for spec in triggers {
        let index = spec.id as usize;
        if index >= outcomes.len() {
            return Err(CycleError::InvalidTriggerId { id: spec.id }.into());
        }
        let Some(slot) = due_slot(ctx, spec)? else {
            continue;
        };
        // A pending prerequisite takes precedence: this slot must remain pending
        // even if a different prerequisite consumed an ordinary failure.
        if spec
            .depends_on
            .iter()
            .any(|id| outcomes[id.as_u32() as usize] == WalkOutcome::Pending)
        {
            record_deferred(ctx, spec, slot, 2)?;
            outcomes[index] = WalkOutcome::Pending;
        } else if spec
            .depends_on
            .iter()
            .any(|id| outcomes[id.as_u32() as usize] == WalkOutcome::ConsumedFailure)
        {
            record_slot(ctx, spec, slot.recorded_at)?;
            outcomes[index] = WalkOutcome::ConsumedFailure;
        } else if spec.work_budget_gas() > reserve.available(ctx)? {
            record_deferred(ctx, spec, slot, 1)?;
            outcomes[index] = WalkOutcome::Pending;
        } else {
            let before = ctx.storage.gas_used()?;
            let result = ctx
                .storage
                .with_trigger_budget(spec.work_budget_gas(), || spec.handler.run(ctx));
            let consumed = ctx
                .storage
                .gas_used()?
                .checked_sub(before)
                .ok_or(CycleError::InsufficientWorkCapacity)?;
            reserve.handler_gas = reserve
                .handler_gas
                .checked_add(consumed)
                .ok_or(CycleError::InsufficientWorkCapacity)?;
            #[cfg(feature = "test-utils")]
            TRIGGER_GAS_SAMPLES.with(|samples| samples.borrow_mut().push((spec.id, consumed)));
            match result {
                Ok(()) => {
                    record_executed(ctx, spec, slot)?;
                    outcomes[index] = WalkOutcome::Executed;
                }
                Err(PrecompileError::TriggerBudgetExceeded {
                    consumed_gas,
                    budget_gas,
                }) => {
                    let mut cycle = ctx.storage.contract::<Cycle<'_>>();
                    cycle.emit(ICycle::CycleTriggerOverBudget {
                        id: spec.id,
                        consumedGas: consumed_gas,
                        budgetGas: budget_gas,
                        blockNumber: ctx.block.block_number,
                    })?;
                    outcomes[index] = WalkOutcome::Pending;
                }
                Err(err @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))) => {
                    record_failed(ctx, spec, slot, &err)?;
                    outcomes[index] = WalkOutcome::ConsumedFailure;
                }
                Err(err) => return Err(err),
            }
        }
        reserve.available(ctx)?;
    }
    reserve.available(ctx)?;
    Ok(())
}

fn record_deferred(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    slot: DueSlot,
    reason: u8,
) -> Result<()> {
    let mut cycle = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTriggerDeferred {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockNumber: ctx.block.block_number,
        reason,
    })?;
    Ok(())
}

/// Writes the consumed slot outside the handler checkpoint.
fn record_slot(ctx: &BlockRuntimeContext, spec: &TriggerSpec, recorded_at: u64) -> Result<()> {
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, recorded_at)?;
    cycle
        .last_executed_block_number
        .write(&spec.id, ctx.block.block_number)?;
    Ok(())
}

/// Records a successful slot and emits `CycleTriggerExecuted`.
fn record_executed(ctx: &BlockRuntimeContext, spec: &TriggerSpec, slot: DueSlot) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTriggerExecuted {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::info!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        "cycle trigger fired"
    );
    Ok(())
}

/// Records a reverted slot and emits `CycleTriggerFailed` with no error text.
fn record_failed(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    slot: DueSlot,
    err: &PrecompileError,
) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTriggerFailed {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::error!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        error = ?err,
        "cycle trigger handler reverted; checkpoint rolled back and the slot was consumed"
    );
    Ok(())
}

/// Slot that one trigger fires in the current block.
#[derive(Clone, Copy, Debug)]
struct DueSlot {
    /// Next slot after `last_executed_at`.
    scheduled_at: u64,
    /// Slot that the dispatcher records as `last_executed_at`.
    recorded_at: u64,
}

/// Returns the slot that `spec` fires in the current block, or `None` when
/// the trigger does not fire.
fn due_slot(ctx: &BlockRuntimeContext, spec: &TriggerSpec) -> Result<Option<DueSlot>> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    let last_executed_at = cycle.last_executed_at.read(&spec.id)?;

    // First-ever encounter for this trigger: anchor `last_executed_at`
    // at the current block timestamp. The first real fire is then the
    // next slot strictly after this point. Without this anchor,
    // every chain would fire on its first block because real genesis
    // timestamps are far beyond the first unix-epoch schedule slot.
    if last_executed_at == 0 {
        anchor_first_encounter(ctx, spec)?;
        return Ok(None);
    }

    let scheduled_at = next_fire_at(
        spec.period_seconds(),
        spec.start_offset_seconds,
        last_executed_at,
    )?;
    if block_ts < scheduled_at || slot_deferred(ctx, spec, &cycle)? {
        return Ok(None);
    }

    // A poll records the latest due slot, so a gap costs one firing rather
    // than one per missed slot.
    let recorded_at = if spec.coalesces_backlog {
        last_fire_at(spec.period_seconds(), spec.start_offset_seconds, block_ts)?
    } else {
        scheduled_at
    };
    Ok(Some(DueSlot {
        scheduled_at,
        recorded_at,
    }))
}

/// Writes the current block timestamp as the first `last_executed_at` of `spec`.
fn anchor_first_encounter(ctx: &BlockRuntimeContext, spec: &TriggerSpec) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, block_ts)?;
    tracing::debug!(
        target: "outbe::cycle",
        trigger_id = spec.id,
        label = spec.label,
        block_ts,
        "cycle trigger anchored on first encounter; first fire deferred to next slot"
    );
    Ok(())
}

/// Returns `true` when a due slot of `spec` must stay pending until a later
/// block. The deferral changes no state.
fn slot_deferred(ctx: &BlockRuntimeContext, spec: &TriggerSpec, cycle: &Cycle<'_>) -> Result<bool> {
    // refuse to fire a gated trigger until Phase 1
    // has accounted the parent block. Under the V2 reorder,
    // Phase 1 commits BEFORE Phase 2 (`CycleTick`). Thus
    // this gate is normally vacuously satisfied. It fires only when
    // a regression reorders the phases or a new trigger reads state
    // that races the parent-finalization tx. Defer silently (no
    // error, no state change), so the trigger retries on the next
    // block.
    let progress = EvmAccountingProgress::new(ctx);
    if accounting_gate_blocks(spec, &progress, &ctx.block)? {
        tracing::debug!(
            target: "outbe::cycle",
            trigger_id = spec.id,
            label = spec.label,
            block_number = ctx.block.block_number,
            "cycle trigger deferred: Phase 1 has not yet accounted the parent block"
        );
        return Ok(true);
    }
    protocol_cycle_awaits_late_window(ctx, spec, cycle)
}

/// At midnight, keep the protocol slot pending until the previous day's final
/// late-vote window has executed. Other triggers retain their own cadence.
/// This gate uses no sleep or wall-clock timer.
fn protocol_cycle_awaits_late_window(
    ctx: &BlockRuntimeContext,
    spec: &TriggerSpec,
    _cycle: &Cycle<'_>,
) -> Result<bool> {
    if spec.id != TriggerId::ProtocolCycle.as_u32() {
        return Ok(false);
    }
    let active_utc_day = crate::handler::derive_active_utc_day(
        ctx.storage.clone(),
        ctx.block.genesis_timestamp,
        ctx.block.timestamp,
    )?;
    let day_action = protocol_day_action(
        active_utc_day,
        outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
    )?;
    match day_action {
        ProtocolDayAction::SettlePrevious { day } => {
            Ok(!outbe_rewards::api::is_voting_complete(ctx, day)?)
        }
        ProtocolDayAction::SameDay => Ok(false),
    }
}
