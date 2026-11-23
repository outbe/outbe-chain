use crate::executor::{
    BlockExecutionDependencies, BlockExecutionIdentity, BlockExecutorInputs, BlockSystemPlan,
    OutbeBlockExecutor, ParentAccountingInputs,
};
use crate::factory::{OutbeEvm, OutbeEvmFactory, OutbeEvmTypes};
use evm2::evm::DynDatabase;
use reth_ethereum::{
    evm::{EthBlockExecutor, EthEvmEnv},
    Receipt, TransactionSigned,
};
use reth_evm::BlockExecutorFactory;
use std::sync::Arc;

use super::{OutbeBlockExecutionCtx, OutbeEvmConfig};

// ---------------------------------------------------------------------------
// BlockExecutorFactory
// ---------------------------------------------------------------------------

impl BlockExecutorFactory for OutbeEvmConfig {
    type EvmFactory = OutbeEvmFactory;
    type EvmTypes = OutbeEvmTypes;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm<'a> = OutbeEvm<'a>;
    type EvmEnv = EthEvmEnv<OutbeEvmTypes>;
    type ExecutionCtx<'a>
        = OutbeBlockExecutionCtx<'a>
    where
        Self: 'a;
    type Executor<'a>
        = OutbeBlockExecutor<'a>
    where
        Self: 'a;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn evm_with_env<'a, DB>(&self, db: DB, evm_env: Self::EvmEnv) -> Self::Evm<'a>
    where
        DB: DynDatabase + 'a,
    {
        self.evm_factory.create_evm(db, evm_env)
    }

    fn create_executor<'a>(
        &'a self,
        mut evm: Self::Evm<'a>,
        ctx: OutbeBlockExecutionCtx<'a>,
    ) -> Self::Executor<'a>
    where
        Self: 'a,
    {
        evm.ext_mut().set_owned_by_executor(true);
        let block_extra_data = ctx.inner.extra_data.clone();
        let block_hash = ctx.block_hash;
        let parent_hash = ctx.inner.parent_hash;
        let withdrawals = ctx.inner.withdrawals.as_deref().map(<[_]>::to_vec);
        let mut inner_ctx = ctx.inner;
        // Outbe validates native withdrawals via `validate_outbe_withdrawals` and must not apply
        // Ethereum's Gwei-to-Wei balance-increment conversion in post-execution.
        inner_ctx.withdrawals = None;

        let system_plan = BlockSystemPlan {
            expected_begin_system_txs: ctx.expected_begin_system_txs,
            system_layout_error: ctx.system_layout_error,
            proposer_evm_address: ctx.proposer_evm_address,
            execute_outbe_block_hooks: ctx.execute_outbe_block_hooks,
            prebuilt_phase1_tx: ctx.prebuilt_phase1_tx,
            pending_tee_bootstrap: ctx.pending_tee_bootstrap,
        };

        let inner = EthBlockExecutor::new(
            evm,
            inner_ctx,
            self.chain_spec.as_ref(),
            &self.receipt_builder,
        );
        OutbeBlockExecutor::new(
            inner,
            Arc::clone(&self.chain_spec),
            withdrawals,
            BlockExecutorInputs {
                identity: BlockExecutionIdentity {
                    block_extra_data,
                    validate_execution_summary: true,
                    block_hash,
                    block_state_root: ctx.block_state_root,
                    parent_hash,
                },
                system_plan,
                parent_accounting: ParentAccountingInputs {
                    accounted_parent_artifact_provider: self
                        .accounted_parent_artifact_provider
                        .clone(),
                    parent_consensus_metadata: ctx.parent_consensus_metadata,
                    parent_artifact_hint: ctx.parent_artifact_hint,
                },
                dependencies: BlockExecutionDependencies {
                    bridge: self.bridge.clone(),
                    evm_signer: self.evm_signer.clone(),
                    runtime_host: Arc::clone(&self.runtime_host),
                },
            },
        )
    }
}
