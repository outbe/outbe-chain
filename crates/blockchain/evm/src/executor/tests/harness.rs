use super::{OutbeBlockExecutor, B256};

// test-only opt-out: scoped flag that disables the Phase 1
// `verify_v2_proof` preflight in `apply_pre_execution_changes`. The flag
// is thread-local and one-shot per test; production code paths never set
// it. See `with_phase1_verify_disabled`.
#[cfg(test)]
thread_local! {
    pub(in crate::executor) static PHASE1_VERIFY_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only guard that disables the Phase 1 `verify_v2_proof` preflight
/// for the duration of `f`. Production code paths never call this.
#[cfg(test)]
pub(crate) fn with_phase1_verify_disabled<R>(f: impl FnOnce() -> R) -> R {
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(true));
    let result = f();
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(false));
    result
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    #[cfg(test)]
    pub(crate) fn force_preexecuted_phase1_witness_for_test(&mut self, tx_hash: B256) {
        self.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            tx_hash,
            receipt_index: 0,
        };
    }
}
