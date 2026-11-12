//! Thread-local execution context and test hooks for system transactions.

#[cfg(any(test, feature = "test-hooks"))]
use alloy_primitives::{Address, U256};
use outbe_primitives::block::BlockContext;
pub use outbe_primitives::system_tx_context::SystemTxExecutionContext as PreloadedSystemTxContext;

thread_local! {
    static PRELOADED_SYSTEM_TX_CONTEXT: std::cell::RefCell<Option<PreloadedSystemTxContext>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(any(test, feature = "test-hooks"))]
    static OBSERVED_RUNTIME_BLOCK_CONTEXTS: std::cell::RefCell<Vec<BlockContext>> =
        const { std::cell::RefCell::new(Vec::new()) };
    #[cfg(any(test, feature = "test-hooks"))]
    static FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT: std::cell::RefCell<Option<(Address, U256, U256)>> =
        const { std::cell::RefCell::new(None) };
}

/// Takes and clears all block contexts observed by the system-tx precompile on this thread.
#[cfg(any(test, feature = "test-hooks"))]
pub fn take_observed_runtime_block_contexts() -> Vec<BlockContext> {
    OBSERVED_RUNTIME_BLOCK_CONTEXTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Records one built block context for tests that assert end-block observation.
/// Production builds drop the value.
pub fn record_observed_runtime_block_context(block: &BlockContext) {
    #[cfg(any(test, feature = "test-hooks"))]
    OBSERVED_RUNTIME_BLOCK_CONTEXTS.with(|slot| slot.borrow_mut().push(block.clone()));
    #[cfg(not(any(test, feature = "test-hooks")))]
    {
        let _ = block;
    }
}

/// Runs `f` with explicit non-calldata context visible to the system
/// precompile on the current thread.
///
/// This keeps CertifiedParentAccounting money fields out of signed calldata. It
/// still gives the precompile an explicit deterministic data path for the
/// parent block's committed execution summary. The executor sets this only
/// around a single `transact_system_call`, and the guard restores the previous
/// value on exit.
pub fn with_preloaded_system_tx_context<R>(
    context: PreloadedSystemTxContext,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset(Option<PreloadedSystemTxContext>);

    impl Drop for Reset {
        fn drop(&mut self) {
            PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| {
                *slot.borrow_mut() = self.0.take();
            });
        }
    }

    let previous = PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.replace(Some(context)));
    let _reset = Reset(previous);
    f()
}

/// Returns the preloaded system-transaction execution context on the current thread, if any.
pub fn current_preloaded_system_tx_context() -> Option<PreloadedSystemTxContext> {
    PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.borrow().clone())
}

/// Test hook read by the runtime system-transaction dispatch.
/// Production builds return `None`.
#[cfg(any(test, feature = "test-hooks"))]
pub fn forced_dirty_system_precompile_revert() -> Option<(Address, U256, U256)> {
    FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| *cell.borrow())
}

/// Runs `f` with a forced dirty revert configured on the system-transaction precompile.
#[cfg(any(test, feature = "test-hooks"))]
pub fn with_forced_dirty_system_precompile_revert<R>(
    address: Address,
    slot: U256,
    value: U256,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
    }

    FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
        *cell.borrow_mut() = Some((address, slot, value));
    });
    let _reset = Reset;
    f()
}
