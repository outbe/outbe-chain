//! Test-only controls for block execution. Production builds omit this module.
//! Runtime integration tests enable it with the `test-hooks` feature.

use alloy_primitives::B256;

use super::OutbeBlockExecutor;

thread_local! {
    pub static PHASE1_VERIFY_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Disables the Phase 1 certificate preflight for the duration of `f`.
pub fn with_phase1_verify_disabled<R>(f: impl FnOnce() -> R) -> R {
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(true));
    let result = f();
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(false));
    result
}

impl<Evm> OutbeBlockExecutor<'_, Evm> {
    /// Marks Phase 1 as already executed so a detached replay can observe it.
    pub fn force_preexecuted_phase1_witness_for_test(&mut self, tx_hash: B256) {
        self.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            tx_hash,
            receipt_index: 0,
        };
    }
}
