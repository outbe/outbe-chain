//! EVM-level witness scope attachment for direct RPC, prewarm, and historical trace replay.

use crate::executor::PreloadedSystemTxContext;
use alloy_primitives::{Bytes, Log, TxKind};
use outbe_tee::witness::{
    EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use reth_ethereum::evm::revm::{
    context::TxEnv,
    context_interface::result::{
        ExecutionResult, HaltReason, Output, ResultAndState, ResultGas, SuccessReason,
    },
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct DetachedTraceExecutionMeta {
    pub expected_begin_inputs: Vec<Bytes>,
    pub expected_tx_count: Option<usize>,
    pub phase1_preexecuted: bool,
    pub system_tx_context: Option<PreloadedSystemTxContext>,
}

#[derive(Clone)]
struct DetachedTraceWitnessContext {
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
    state: Arc<Mutex<WitnessState>>,
    meta: DetachedTraceExecutionMeta,
}

thread_local! {
    static DETACHED_TRACE_WITNESS: std::cell::RefCell<Option<DetachedTraceWitnessContext>> =
        const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Copy)]
pub struct EvmTransactWitnessInput<'a> {
    pub attached_state: Option<&'a Arc<Mutex<WitnessState>>>,
    pub owned_by_executor: bool,
    pub precompiles_mut_called: bool,
    pub chain_id: u64,
    pub block_number: u64,
    pub block_timestamp: u64,
}

pub enum DetachedTraceTxAction {
    NormalTransact,
    SkipPreexecutedPhase1 {
        gas_limit: u64,
    },
    ReservedSystemCall {
        data: Bytes,
        context: Box<PreloadedSystemTxContext>,
    },
}

pub struct MatchedDetachedTraceTx {
    pub state: Arc<Mutex<WitnessState>>,
    pub expected_tx_count: Option<usize>,
    pub action: DetachedTraceTxAction,
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn install_detached_trace_witness(
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
    state: Option<Arc<Mutex<WitnessState>>>,
) {
    DETACHED_TRACE_WITNESS.with(|slot| {
        *slot.borrow_mut() = state.map(|state| DetachedTraceWitnessContext {
            chain_id,
            block_number,
            block_timestamp,
            state,
            meta: DetachedTraceExecutionMeta::default(),
        });
    });
}

pub fn install_detached_trace_witness_with_meta(
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
    state: Option<Arc<Mutex<WitnessState>>>,
    meta: DetachedTraceExecutionMeta,
) {
    let detached_state = state.and_then(|state_arc| {
        let guard = state_arc.lock().ok()?;
        let mut cloned = guard.clone();
        if cloned.mode == WitnessExecutionMode::VerifierWithEnclave {
            cloned.mode = WitnessExecutionMode::VerifierReplay;
        }
        Some(Arc::new(Mutex::new(cloned)))
    });
    DETACHED_TRACE_WITNESS.with(|slot| {
        *slot.borrow_mut() = detached_state.map(|state| DetachedTraceWitnessContext {
            chain_id,
            block_number,
            block_timestamp,
            state,
            meta,
        });
    });
}

pub fn clear_detached_trace_witness() {
    DETACHED_TRACE_WITNESS.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// Accumulates user-transaction gas in the detached trace context so `EndBlock`
/// system transactions replayed via `TxTracer` observe the exact `MidBlock`
/// `user_gas_used` snapshot.
pub fn record_detached_trace_user_gas(user_gas: u64) {
    DETACHED_TRACE_WITNESS.with(|slot| {
        if let Some(context) = slot
            .borrow_mut()
            .as_mut()
            .and_then(|trace| trace.meta.system_tx_context.as_mut())
        {
            context.user_gas_used = context.user_gas_used.saturating_add(user_gas);
        }
    });
}

pub fn synthetic_trace_success(gas_limit: u64, logs: Vec<Log>) -> ResultAndState<HaltReason> {
    ResultAndState::new(
        ExecutionResult::Success {
            reason: SuccessReason::Stop,
            gas: ResultGas::new_with_state_gas(gas_limit, 0, 0, 0),
            logs,
            output: Output::Call(Bytes::new()),
        },
        Default::default(),
    )
}

fn is_still_active_trace_cursor(ctx: &DetachedTraceWitnessContext, idx: usize) -> bool {
    if ctx.meta.expected_tx_count.is_some_and(|count| idx >= count) {
        return false;
    }
    if idx < ctx.meta.expected_begin_inputs.len() {
        return true;
    }
    let Ok(guard) = ctx.state.lock() else {
        return false;
    };
    if guard.tx_index.is_none() {
        return true;
    }
    guard.tx_cursor < guard.tape.txs.len() || ctx.meta.expected_tx_count.is_some()
}

fn trace_action_for_tx(
    meta: &DetachedTraceExecutionMeta,
    idx: usize,
    is_system_call: bool,
    tx: &TxEnv,
) -> DetachedTraceTxAction {
    if !is_system_call {
        return DetachedTraceTxAction::NormalTransact;
    }
    if idx == 0 && meta.phase1_preexecuted {
        return DetachedTraceTxAction::SkipPreexecutedPhase1 {
            gas_limit: tx.gas_limit,
        };
    }
    match meta.system_tx_context.clone() {
        Some(context) => DetachedTraceTxAction::ReservedSystemCall {
            data: tx.data.clone(),
            context: Box::new(context),
        },
        None => DetachedTraceTxAction::NormalTransact,
    }
}

fn active_trace_cursor_for_input(
    ctx: &DetachedTraceWitnessContext,
    input: &EvmTransactWitnessInput<'_>,
) -> Option<usize> {
    if ctx.chain_id != input.chain_id
        || ctx.block_number != input.block_number
        || ctx.block_timestamp != input.block_timestamp
    {
        return None;
    }
    let idx = ctx
        .state
        .lock()
        .ok()
        .and_then(|guard| guard.tx_index)
        .unwrap_or(0) as usize;
    is_still_active_trace_cursor(ctx, idx).then_some(idx)
}

pub fn match_detached_trace_tx(
    input: &EvmTransactWitnessInput<'_>,
    tx: &TxEnv,
) -> Option<MatchedDetachedTraceTx> {
    if input.owned_by_executor || input.precompiles_mut_called || input.attached_state.is_some() {
        return None;
    }
    DETACHED_TRACE_WITNESS.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let ctx = borrow.as_ref()?;
        let Some(idx) = active_trace_cursor_for_input(ctx, input) else {
            *borrow = None;
            return None;
        };
        let is_system_call =
            tx.kind == TxKind::Call(outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS);
        if ctx
            .meta
            .expected_begin_inputs
            .get(idx)
            .is_some_and(|expected_input| !is_system_call || &tx.data != expected_input)
        {
            *borrow = None;
            return None;
        }
        let action = trace_action_for_tx(&ctx.meta, idx, is_system_call, tx);
        Some(MatchedDetachedTraceTx {
            state: ctx.state.clone(),
            expected_tx_count: ctx.meta.expected_tx_count,
            action,
        })
    })
}

fn matching_detached_trace_witness(
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
) -> Option<Arc<Mutex<WitnessState>>> {
    DETACHED_TRACE_WITNESS.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let ctx = borrow.as_ref()?;
        if ctx.chain_id != chain_id
            || ctx.block_number != block_number
            || ctx.block_timestamp != block_timestamp
        {
            *borrow = None;
            return None;
        }
        let idx = ctx
            .state
            .lock()
            .ok()
            .and_then(|guard| guard.tx_index)
            .unwrap_or(0) as usize;
        if !is_still_active_trace_cursor(ctx, idx) {
            *borrow = None;
            return None;
        }
        Some(ctx.state.clone())
    })
}

fn prepare_attached_witness_tx(guard: &mut WitnessState) {
    let idx = *guard.tx_index.get_or_insert(0);
    if matches!(
        guard.mode,
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay
    ) {
        while guard
            .tape
            .txs
            .get(guard.tx_cursor)
            .is_some_and(|tx| tx.tx_index < idx)
        {
            guard.tx_cursor += 1;
        }
    }
    guard.is_user_candidate = false;
    guard.replay_cursor = 0;
    guard.staged.clear();
    guard.candidate_rejection = None;
}

fn finish_attached_witness_tx(guard: &mut WitnessState) {
    let Some(idx) = guard.tx_index else {
        return;
    };
    if matches!(
        guard.mode,
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay
    ) && guard
        .tape
        .txs
        .get(guard.tx_cursor)
        .is_some_and(|tx| tx.tx_index == idx && tx.entries.len() == guard.replay_cursor)
    {
        guard.tx_cursor += 1;
    }
    guard.replay_cursor = 0;
    guard.staged.clear();
    guard.tx_index = Some(idx.saturating_add(1));
}

fn run_with_attached_evm_witness<R>(state: Arc<Mutex<WitnessState>>, f: impl FnOnce() -> R) -> R {
    if let Ok(mut guard) = state.lock() {
        prepare_attached_witness_tx(&mut guard);
    }
    let scope = WitnessExecutionScope::enter(state.clone());
    let output = f();
    drop(scope);
    if let Ok(mut guard) = state.lock() {
        finish_attached_witness_tx(&mut guard);
    }
    output
}

pub fn run_matched_detached_trace_tx<R>(
    matched: MatchedDetachedTraceTx,
    f: impl FnOnce(DetachedTraceTxAction) -> R,
) -> R {
    let state = matched.state;
    let expected_tx_count = matched.expected_tx_count;
    let action = matched.action;
    let output = run_with_attached_evm_witness(state.clone(), || f(action));
    if expected_tx_count.is_some_and(|count| {
        state
            .lock()
            .ok()
            .and_then(|guard| guard.tx_index)
            .is_some_and(|next_idx| next_idx as usize >= count)
    }) {
        clear_detached_trace_witness();
    }
    output
}

pub fn with_evm_transact_witness_scope<R>(
    input: EvmTransactWitnessInput<'_>,
    f: impl FnOnce() -> R,
) -> R {
    if outbe_tee::call_context::current_witness_state().is_some() {
        return f();
    }
    if let Some(state) = input.attached_state.cloned() {
        return run_with_attached_evm_witness(state, f);
    }
    if !input.owned_by_executor && !input.precompiles_mut_called {
        if let Some(state) = matching_detached_trace_witness(
            input.chain_id,
            input.block_number,
            input.block_timestamp,
        ) {
            return run_with_attached_evm_witness(state, f);
        }
        let is_prewarm_thread = std::thread::current()
            .name()
            .is_some_and(|name| name.starts_with("prewarm"));
        if !is_prewarm_thread {
            let rpc_state = Arc::new(Mutex::new(WitnessState::new(
                WitnessExecutionMode::RpcDirect,
                None,
                EnclaveWitnessTapeV1::default(),
            )));
            let _scope = WitnessExecutionScope::enter(rpc_state);
            return f();
        }
    }
    f()
}
