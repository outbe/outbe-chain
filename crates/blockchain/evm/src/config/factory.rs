use crate::{executor::OutbeBlockExecutor, factory::OutbeEvmFactory};
use alloy_evm::{
    block::{BlockExecutorFactory, StateDB},
    Evm,
};
use reth_ethereum::evm::revm::inspector::Inspector;
use reth_ethereum::Receipt;
use reth_ethereum::TransactionSigned;

use reth_evm::EvmFor;

use super::{OutbeBlockExecutionCtx, OutbeEvmConfig};

// ---------------------------------------------------------------------------
// BlockExecutorFactory
// ---------------------------------------------------------------------------

impl BlockExecutorFactory for OutbeEvmConfig {
    /// We keep the same EVM factory as the inner config: it creates
    /// `OutbeEvm<DB, I, PrecompilesMap>` with Outbe precompiles registered.
    type EvmFactory = OutbeEvmFactory;
    type ExecutionCtx<'a> = OutbeBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    /// Per-tx execution result. `OutbeBlockExecutor` wraps reth's
    /// `EthBlockExecutor`, so this is the same `EthTxResult` reth produces:
    /// `EvmFactory = OutbeEvmFactory`, `Transaction = TransactionSigned`
    /// (`TxType` resolves to `reth_ethereum::TxType`).
    type TxExecutionResult = alloy_evm::eth::EthTxResult<
        <OutbeEvmFactory as alloy_evm::EvmFactory>::HaltReason,
        reth_ethereum::TxType,
    >;
    /// Concrete executor type returned by `create_executor`. `OutbeBlockExecutor<'a, E>`
    /// is parameterized by the raw EVM (`BlockExecutor::Evm = E`), which the trait
    /// constrains to `<Self::EvmFactory>::Evm<DB, I>` (i.e. `EvmFor<Self, DB, I>`). The
    /// `EthBlockExecutor` wrapper with `&Arc<ChainSpec<OutbeHeader>>` and
    /// `&RethReceiptBuilder` lives inside `OutbeBlockExecutor`, not in this type param.
    type Executor<
        'a,
        DB: StateDB,
        I: Inspector<<Self::EvmFactory as alloy_evm::EvmFactory>::Context<DB>>,
    > = OutbeBlockExecutor<'a, EvmFor<Self, DB, I>>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        // `ConfigureEvm::evm_factory()` delegates to
        // `block_executor_factory().evm_factory()` which returns `&OutbeEvmFactory`.
        self.inner.executor_factory.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmFor<Self, DB, I>,
        ctx: OutbeBlockExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: Inspector<<Self::EvmFactory as alloy_evm::EvmFactory>::Context<DB>>,
    {
        use alloy_evm::eth::EthBlockExecutor;
        let block_extra_data = ctx.inner.extra_data.clone();
        let block_hash = ctx.block_hash;
        let parent_hash = ctx.inner.parent_hash;
        let expected_begin_system_txs = ctx.expected_begin_system_txs.clone();
        let expected_end_system_txs = ctx.expected_end_system_txs.clone();
        let mut system_layout_error = ctx.system_layout_error.clone();
        let parent_consensus_metadata = ctx.parent_consensus_metadata.clone();
        let proposer_evm_address = ctx.proposer_evm_address;
        let execute_outbe_block_hooks = ctx.execute_outbe_block_hooks;
        let prebuilt_phase1_tx = ctx.prebuilt_phase1_tx.clone();
        let parent_artifact_hint = ctx.parent_artifact_hint;
        let pending_tee_bootstrap = ctx.pending_tee_bootstrap.clone();
        let runtime_body_readers = evm.runtime_body_readers().cloned();
        let block_number = evm.block().number.saturating_to::<u64>();
        let compressed_entities_scope = evm.execution_scope().clone();
        if let Err(error) = self.configure_compressed_entities_scope(
            &compressed_entities_scope,
            block_number,
            parent_hash,
        ) {
            system_layout_error.get_or_insert_with(|| {
                format!("compressed-entity exact-parent scope configuration failed: {error}")
            });
        }
        let execution_read_budget = ctx.execution_read_budget.clone();

        OutbeBlockExecutor::new(
            EthBlockExecutor::new(
                evm,
                ctx.inner,
                self.inner.chain_spec(),
                self.inner.executor_factory.receipt_builder(),
            ),
            self.bridge.clone(),
            block_extra_data,
            self.accounted_parent_artifact_provider.clone(),
            true,
            block_hash,
            parent_hash,
            self.evm_signer.clone(),
            expected_begin_system_txs,
            expected_end_system_txs,
            system_layout_error,
            parent_consensus_metadata,
            proposer_evm_address,
            execute_outbe_block_hooks,
            prebuilt_phase1_tx,
            parent_artifact_hint,
        )
        .with_block_state_root(ctx.block_state_root)
        .with_compressed_entities_scope(compressed_entities_scope)
        .with_compressed_tree_service(self.compressed_tree_service.clone())
        .with_runtime_body_readers(runtime_body_readers, execution_read_budget)
        .with_pending_tee_bootstrap(pending_tee_bootstrap)
        .with_ocomp_lifecycle_active(self.ocomp_lifecycle_active_at(block_number))
    }
}
