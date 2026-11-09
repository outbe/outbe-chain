use alloy_primitives::Address;
use outbe_primitives::block::BlockContext;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::system_tx_context::AccountedParentArtifact;

pub(crate) use outbe_primitives::system_tx_context::SystemTxExecutionContext as PreloadedSystemTxContext;

thread_local! {
    static PRELOADED_SYSTEM_TX_CONTEXT: std::cell::RefCell<Option<PreloadedSystemTxContext>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(test)]
    static OBSERVED_RUNTIME_BLOCK_CONTEXTS: std::cell::RefCell<Vec<BlockContext>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_observed_runtime_block_contexts() -> Vec<BlockContext> {
    OBSERVED_RUNTIME_BLOCK_CONTEXTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Runs `f` with explicit non-calldata context visible to the system
/// precompile on the current thread.
///
/// This keeps CertifiedParentAccounting money fields out of signed calldata. It
/// still gives the precompile an explicit deterministic data path for the
/// parent block's committed execution summary. The executor sets this only
/// around a single `transact_system_call`, and the guard restores the previous
/// value on exit.
pub(crate) fn with_preloaded_system_tx_context<R>(
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

pub(crate) fn current_preloaded_system_tx_context() -> Option<PreloadedSystemTxContext> {
    PRELOADED_SYSTEM_TX_CONTEXT.with(|slot| slot.borrow().clone())
}

pub(super) fn block_runtime_context_from_storage(
    storage: StorageHandle,
    include_validator_snapshot: bool,
) -> Result<BlockRuntimeContext> {
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
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let mut validators: Vec<Address> = vs
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        validators.sort();
        validators
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
    #[cfg(test)]
    OBSERVED_RUNTIME_BLOCK_CONTEXTS.with(|slot| slot.borrow_mut().push(block_ctx.clone()));

    Ok(BlockRuntimeContext::new(block_ctx, storage))
}

pub(crate) fn read_preloaded_finalized_summary(
    _storage: &StorageHandle<'_>,
) -> Result<Option<AccountedParentArtifact>> {
    Ok(current_preloaded_system_tx_context().and_then(|context| context.finalized_summary))
}
