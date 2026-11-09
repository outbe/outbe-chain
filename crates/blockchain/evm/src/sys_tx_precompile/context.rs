use outbe_primitives::block::BlockContext;
#[cfg(any(feature = "test-hooks", feature = "test-utils"))]
use outbe_primitives::block::BlockRuntimeContext;
#[cfg(any(feature = "test-hooks", feature = "test-utils"))]
use outbe_primitives::error::Result;
#[cfg(any(feature = "test-hooks", feature = "test-utils"))]
use outbe_primitives::storage::StorageHandle;
pub use outbe_primitives::system_tx_context::SystemTxExecutionContext as PreloadedSystemTxContext;

thread_local! {
    static PRELOADED_SYSTEM_TX_CONTEXT: std::cell::RefCell<Option<PreloadedSystemTxContext>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(any(test, feature = "test-hooks"))]
    static OBSERVED_RUNTIME_BLOCK_CONTEXTS: std::cell::RefCell<Vec<BlockContext>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

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

pub fn current_preloaded_system_tx_context() -> Option<PreloadedSystemTxContext> {
    PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.borrow().clone())
}

#[cfg(any(test, feature = "test-hooks", feature = "test-utils"))]
pub fn block_runtime_context_from_storage<'a>(
    host: &dyn crate::EvmRuntimeHost,
    storage: StorageHandle<'a>,
    include_validator_snapshot: bool,
) -> Result<BlockRuntimeContext<'a>> {
    let block_number = storage.block_number()?;
    let timestamp = storage.now_secs()?;
    let chain_id = storage.chain_id()?;
    let genesis_hash = storage.genesis_hash()?;
    let preloaded = current_preloaded_system_tx_context();
    let proposer = preloaded
        .as_ref()
        .map(|context| context.proposer)
        .unwrap_or(storage.beneficiary()?);
    let block_gas_limit = preloaded
        .as_ref()
        .map(|context| context.block_gas_limit)
        .unwrap_or(0);
    let user_gas_used = preloaded
        .as_ref()
        .map(|context| context.user_gas_used)
        .unwrap_or(0);

    let validators = if include_validator_snapshot {
        host.consensus_validators(storage.clone())?
    } else {
        Vec::new()
    };

    let block_ctx = BlockContext {
        block_gas_limit,
        user_gas_used,
        block_number,
        timestamp,
        chain_id,
        genesis_hash,
        genesis_timestamp: storage.genesis_timestamp()?,
        proposer,
        validators,
    };
    record_observed_runtime_block_context(&block_ctx);

    Ok(BlockRuntimeContext::new(block_ctx, storage))
}
