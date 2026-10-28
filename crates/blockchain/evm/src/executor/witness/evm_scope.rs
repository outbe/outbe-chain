//! EVM-level witness scope attachment for direct RPC, prewarm, and historical trace replay.

use outbe_tee::witness::{
    EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct DetachedTraceWitnessContext {
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
    state: Arc<Mutex<WitnessState>>,
}

thread_local! {
    static DETACHED_TRACE_WITNESS: std::cell::RefCell<Option<DetachedTraceWitnessContext>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) struct EvmTransactWitnessInput<'a> {
    pub(crate) attached_state: Option<&'a Arc<Mutex<WitnessState>>>,
    pub(crate) owned_by_executor: bool,
    pub(crate) precompiles_mut_called: bool,
    pub(crate) chain_id: u64,
    pub(crate) block_number: u64,
    pub(crate) block_timestamp: u64,
}

pub(crate) fn install_detached_trace_witness(
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
        });
    });
}

pub(crate) fn clear_detached_trace_witness() {
    DETACHED_TRACE_WITNESS.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

fn matching_detached_trace_witness(
    chain_id: u64,
    block_number: u64,
    block_timestamp: u64,
) -> Option<Arc<Mutex<WitnessState>>> {
    DETACHED_TRACE_WITNESS.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|ctx| {
                ctx.chain_id == chain_id
                    && ctx.block_number == block_number
                    && ctx.block_timestamp == block_timestamp
            })
            .map(|ctx| ctx.state.clone())
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

pub(crate) fn with_evm_transact_witness_scope<R>(
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
