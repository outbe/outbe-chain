//! Per-block task dispatch loop.
//!
//! Runs from [`handle_cycle_tick`] on every
//! block during the begin-zone CycleTick phase. Iterates the genesis-resolved
//! task table
//! and fires any task whose next slot has been reached.

use outbe_primitives::{
    block::{BlockRuntimeContext, MetadosisEntitlementContext, ProtocolModule},
    error::{PrecompileError, Result},
    storage::MetadosisMutationEntitlements,
    system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
};

use crate::errors::CycleError;
use crate::schema::Cycle;
use crate::state::{accounting_gate_blocks, EvmAccountingProgress};
use crate::tasks::{active_tasks, last_fire_at, next_fire_at, Readiness, TaskSpec};
use crate::ICycle;

/// Protocol module for the begin-block cycle step.
pub struct CycleModule;

impl ProtocolModule for CycleModule {
    const NAME: &'static str = "cycle_tick";
    const NEEDS_CYCLE_ACTIVE_UTC_DAY: bool = true;

    fn mutation_entitlements(
        ctx: MetadosisEntitlementContext<'_>,
    ) -> MetadosisMutationEntitlements {
        if !ctx.payload.is_empty() {
            return MetadosisMutationEntitlements::NONE;
        }
        crate::call_authority::cycle_tick_entitlements(
            ctx.chain_id,
            ctx.block_number,
            ctx.timestamp,
            ctx.cycle_active_utc_day,
        )
    }

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        handle_cycle_tick(ctx, GENESIS_BOOTSTRAP_BLOCK_NUMBER)
    }
}

#[cfg(feature = "test-utils")]
std::thread_local! {
    static TASK_GAS_SAMPLES: std::cell::RefCell<Vec<(u32, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    static CYCLE_GAS_SAMPLES: std::cell::RefCell<Vec<(u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Drains gross handler-only gas samples on this test thread. Failed handlers
/// retain spent gas, so samples include ordinary reverts as well as successes.
#[cfg(feature = "test-utils")]
pub fn take_task_gas_samples() -> Vec<(u32, u64)> {
    TASK_GAS_SAMPLES.with(|samples| core::mem::take(&mut *samples.borrow_mut()))
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
) -> Result<()> {
    handle_cycle_tick_with_tasks(ctx, metadosis_genesis_activation_height, &active_tasks()?)
}

/// Runs the CycleTick body for a supplied task slice, including bootstrap,
/// reward slicing, and bounded auction cleanup.
pub fn handle_cycle_tick_with_tasks(
    ctx: &BlockRuntimeContext<'_>,
    metadosis_genesis_activation_height: u64,
    tasks: &[TaskSpec],
) -> Result<()> {
    #[cfg(feature = "test-utils")]
    let observation_before = ctx.storage.gas_used()?;
    if ctx.block.block_number == metadosis_genesis_activation_height {
        outbe_tributefactory::worldwideday::bootstrap_genesis_worldwide_days_command(ctx)?;
    }
    let handler_gas = run_due_tasks_inner(ctx, tasks)?;
    #[cfg(not(feature = "test-utils"))]
    let _ = handler_gas;
    crate::handler::process_agent_rewards(ctx)?;
    outbe_desis::process_cleanup(ctx.storage.clone())?;
    #[cfg(feature = "test-utils")]
    record_cycle_gas_sample(ctx, observation_before, handler_gas)?;
    Ok(())
}

/// Dispatches every active task whose `next_fire_at` is `<=
/// ctx.block.timestamp`. Each handler runs in an atomic work-budget scope.
/// Ordinary reverts consume the slot; budget exhaustion rolls back the handler
/// and leaves it and its due dependents pending. Independent work can continue.
/// Other errors reject CycleTick.
///
/// The typical case is a slow-running chain that produces blocks every few
/// seconds. In that case the dispatcher is a near-noop on every block. It
/// fires ProcessEmissionDay only on the first block whose timestamp crosses
/// the next UTC-hour boundary.
pub fn run_due_tasks(ctx: &BlockRuntimeContext) -> Result<()> {
    run_due_tasks_with_specs(ctx, &active_tasks()?)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkOutcome {
    NotRun,
    Executed,
    ConsumedFailure,
    Pending,
}

/// Explicit scheduler for a supplied task table.
pub fn run_due_tasks_with_specs(ctx: &BlockRuntimeContext, tasks: &[TaskSpec]) -> Result<()> {
    let _ = run_due_tasks_inner(ctx, tasks)?;
    Ok(())
}

fn run_due_tasks_inner(ctx: &BlockRuntimeContext, tasks: &[TaskSpec]) -> Result<u64> {
    ctx.storage
        .metered_gas_remaining()?
        .ok_or(CycleError::InsufficientWorkCapacity)?;
    let mut outcomes = [WalkOutcome::NotRun; 9];
    let mut handler_gas = 0u64;
    for spec in tasks {
        let index = spec.id as usize;
        if index >= outcomes.len() {
            return Err(CycleError::InvalidTaskId { id: spec.id }.into());
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
            handler_gas = handler_gas
                .checked_add(consumed)
                .ok_or(CycleError::InsufficientWorkCapacity)?;
            #[cfg(feature = "test-utils")]
            TASK_GAS_SAMPLES.with(|samples| samples.borrow_mut().push((spec.id, consumed)));
            match result {
                Ok(Readiness::Done) => {
                    record_executed(ctx, spec, slot)?;
                    outcomes[index] = WalkOutcome::Executed;
                }
                Ok(Readiness::NotReady) => {}
                Err(PrecompileError::TriggerBudgetExceeded {
                    consumed_gas,
                    budget_gas,
                }) => {
                    let mut cycle = ctx.storage.contract::<Cycle<'_>>();
                    cycle.emit(ICycle::CycleTaskOverBudget {
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
    }
    Ok(handler_gas)
}

fn record_deferred(
    ctx: &BlockRuntimeContext,
    spec: &TaskSpec,
    slot: DueSlot,
    reason: u8,
) -> Result<()> {
    let mut cycle = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTaskDeferred {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockNumber: ctx.block.block_number,
        reason,
    })?;
    Ok(())
}

/// Writes the consumed slot outside the handler checkpoint.
fn record_slot(ctx: &BlockRuntimeContext, spec: &TaskSpec, recorded_at: u64) -> Result<()> {
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, recorded_at)?;
    cycle
        .last_executed_block_number
        .write(&spec.id, ctx.block.block_number)?;
    Ok(())
}

/// Records a successful slot and emits `CycleTaskExecuted`.
fn record_executed(ctx: &BlockRuntimeContext, spec: &TaskSpec, slot: DueSlot) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTaskExecuted {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::info!(
        target: "outbe::cycle",
        task_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        "cycle task fired"
    );
    Ok(())
}

/// Records a reverted slot and emits `CycleTaskFailed` with no error text.
fn record_failed(
    ctx: &BlockRuntimeContext,
    spec: &TaskSpec,
    slot: DueSlot,
    err: &PrecompileError,
) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let block_number = ctx.block.block_number;
    record_slot(ctx, spec, slot.recorded_at)?;
    let mut cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.emit(ICycle::CycleTaskFailed {
        id: spec.id,
        scheduledAt: slot.recorded_at,
        blockTimestamp: block_ts,
        blockNumber: block_number,
    })?;
    tracing::error!(
        target: "outbe::cycle",
        task_id = spec.id,
        label = spec.label,
        scheduled_at = slot.scheduled_at,
        recorded_at = slot.recorded_at,
        block_ts,
        block_number,
        error = ?err,
        "cycle task handler reverted; checkpoint rolled back and the slot was consumed"
    );
    Ok(())
}

/// Slot that one task fires in the current block.
#[derive(Clone, Copy, Debug)]
struct DueSlot {
    /// Next slot after `last_executed_at`.
    scheduled_at: u64,
    /// Slot that the dispatcher records as `last_executed_at`.
    recorded_at: u64,
}

/// Returns the slot that `spec` fires in the current block, or `None` when
/// the task does not fire.
fn due_slot(ctx: &BlockRuntimeContext, spec: &TaskSpec) -> Result<Option<DueSlot>> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    let last_executed_at = cycle.last_executed_at.read(&spec.id)?;

    // First-ever encounter for this task: anchor `last_executed_at`
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
    if block_ts < scheduled_at || slot_deferred(ctx, spec)? {
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
fn anchor_first_encounter(ctx: &BlockRuntimeContext, spec: &TaskSpec) -> Result<()> {
    let block_ts = ctx.block.timestamp;
    let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
    cycle.last_executed_at.write(&spec.id, block_ts)?;
    tracing::debug!(
        target: "outbe::cycle",
        task_id = spec.id,
        label = spec.label,
        block_ts,
        "cycle task anchored on first encounter; first fire deferred to next slot"
    );
    Ok(())
}

/// Returns `true` when a due slot of `spec` must stay pending until a later
/// block. The deferral changes no state.
fn slot_deferred(ctx: &BlockRuntimeContext, spec: &TaskSpec) -> Result<bool> {
    // refuse to fire a gated task until Phase 1
    // has accounted the parent block. Under the V2 reorder,
    // Phase 1 commits BEFORE Phase 2 (`CycleTick`). Thus
    // this gate is normally vacuously satisfied. It fires only when
    // a regression reorders the phases or a new task reads state
    // that races the parent-finalization tx. Defer silently (no
    // error, no state change), so the task retries on the next
    // block.
    let progress = EvmAccountingProgress::new(ctx);
    if accounting_gate_blocks(spec, &progress, &ctx.block)? {
        tracing::debug!(
            target: "outbe::cycle",
            task_id = spec.id,
            label = spec.label,
            block_number = ctx.block.block_number,
            "cycle task deferred: Phase 1 has not yet accounted the parent block"
        );
        return Ok(true);
    }
    Ok(false)
}
