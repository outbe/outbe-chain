use alloy_evm::{block::BlockExecutor, Evm, RecoveredTx};
use outbe_primitives::{
    consensus::ConsensusExecutionBridge,
    reshare_artifact::{decode_outbe_block_artifacts, encode_outbe_block_artifacts},
    OutbeHeader, OutbePrimitives,
};
use reth_ethereum::evm::revm::primitives::hardfork::SpecId;
use reth_ethereum::trie::updates::TrieUpdates;
use reth_ethereum::{
    evm::{primitives::Database, revm::db::State},
    TransactionSigned,
};
use reth_evm::execute::{
    BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, BlockExecutionError,
    ExecutorTx,
};
use reth_primitives_traits::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::StateProvider;
use revm::context::BlockEnv;
use revm::database::states::bundle_state::BundleRetention;

use crate::{
    config::{OutbeBlockAssembler, OutbeBlockExecutionCtx},
    executor::OutbeBlockExecutor,
};

pub(crate) fn encode_final_header_artifacts(
    prefinal_extra_data: &[u8],
    execution_summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact,
    timestamp_millis_part: u64,
) -> Result<alloy_primitives::Bytes, BlockExecutionError> {
    let mut artifacts = decode_outbe_block_artifacts(prefinal_extra_data)
        .map_err(crate::errors::ExecutorValidationError::block_artifacts)?;
    artifacts.execution_summary = Some(execution_summary);
    artifacts.timestamp_millis_part = timestamp_millis_part;
    encode_outbe_block_artifacts(&artifacts)
        .map_err(|err| crate::errors::ExecutorValidationError::block_artifacts(err).into())
}

pub struct OutbeBlockBuilder<'a, EVM>
where
    EVM: Evm,
{
    pub executor: OutbeBlockExecutor<'a, EVM>,
    pub transactions: Vec<Recovered<TransactionSigned>>,
    pub ctx: OutbeBlockExecutionCtx<'a>,
    pub bridge: Option<ConsensusExecutionBridge>,
    pub parent: &'a SealedHeader<OutbeHeader>,
    pub assembler: &'a OutbeBlockAssembler,
}

impl<'a, EVM> OutbeBlockBuilder<'a, EVM>
where
    EVM: Evm,
{
    pub fn new(
        executor: OutbeBlockExecutor<'a, EVM>,
        ctx: OutbeBlockExecutionCtx<'a>,
        bridge: Option<ConsensusExecutionBridge>,
        assembler: &'a OutbeBlockAssembler,
        parent: &'a SealedHeader<OutbeHeader>,
    ) -> Self {
        Self {
            executor,
            transactions: Vec::new(),
            ctx,
            bridge,
            parent,
            assembler,
        }
    }

    /// Encodes execution-produced header fields before state-root calculation.
    /// The final finish call repeats the same encoding.
    pub fn finalize_header_artifacts(&mut self) -> Result<(), BlockExecutionError> {
        self.ctx.inner.extra_data = encode_final_header_artifacts(
            self.ctx.inner.extra_data.as_ref(),
            self.executor.current_execution_summary(),
            self.ctx.timestamp_millis_part,
        )?;
        self.executor
            .set_final_extra_data(self.ctx.inner.extra_data.clone());
        Ok(())
    }
}

impl<'a, DB, EVM> BlockBuilder for OutbeBlockBuilder<'a, EVM>
where
    DB: Database + 'a,
    OutbeBlockExecutor<'a, EVM>:
        BlockExecutor<Evm = EVM, Transaction = TransactionSigned, Receipt = reth_ethereum::Receipt>,
    EVM: Evm<
            DB = &'a mut State<DB>,
            Spec = SpecId,
            BlockEnv = BlockEnv,
            Tx = reth_ethereum::evm::revm::context::TxEnv,
        > + crate::executor::ZeroFeeCfgAccess,
{
    type Primitives = OutbePrimitives;
    type Executor = OutbeBlockExecutor<'a, EVM>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.executor.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&<Self::Executor as BlockExecutor>::Result) -> alloy_evm::block::CommitChanges,
    ) -> Result<Option<alloy_evm::block::GasOutput>, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let include_preexecuted_phase1_witness =
            self.executor.is_preexecuted_phase1_witness(tx.tx());

        if let Some(gas_used) = self
            .executor
            .execute_transaction_with_commit_condition((tx_env, &tx), f)?
        {
            self.transactions.push(tx);
            Ok(Some(gas_used))
        } else if include_preexecuted_phase1_witness {
            self.transactions.push(tx);
            Ok(None)
        } else {
            Ok(None)
        }
    }

    fn finish(
        mut self,
        state: impl StateProvider,
        state_root_precomputed: Option<(alloy_primitives::B256, TrieUpdates)>,
    ) -> Result<BlockBuilderOutcome<OutbePrimitives>, BlockExecutionError> {
        // finalized-parent metadata travels through payload
        // attributes into the begin-zone Phase 1 system transaction body.
        // Header `extra_data` here carries the execution summary, timestamp
        // millis and the artifacts kept from
        // the pre-final bytes (DKG/header artifacts and late-finalize credits).
        // The proposer path does not produce the legacy header attestation tag.
        let execution_summary = self.executor.current_execution_summary();
        // Sub-second timestamp travels in `extra_data` under tag 0x05 so
        // the block hash stays Ethereum-spec-compliant
        // (`keccak256(rlp(standard_header))`). The execution-context
        // value is the canonical source for this block. This code overwrites
        // whatever the proposer placed in `extra_data` earlier.
        self.finalize_header_artifacts()?;
        let (evm, result) = self.executor.finish()?;
        let (db, evm_env) = evm.finish();

        db.merge_transitions(BundleRetention::Reverts);

        let hashed_state = state
            .hashed_post_state(&db.bundle_state)
            .map_err(BlockExecutionError::other)?;
        let (state_root, trie_updates) = match state_root_precomputed {
            Some(precomputed) => precomputed,
            None => state
                .state_root_with_updates(hashed_state.clone())
                .map_err(BlockExecutionError::other)?,
        };

        let (transactions, senders): (Vec<_>, Vec<_>) = self
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block = self.assembler.assemble_block(BlockAssemblerInput::<
            crate::config::OutbeEvmConfig,
            OutbeHeader,
        >::new(
            evm_env,
            self.ctx,
            self.parent,
            transactions,
            &result,
            &db.bundle_state,
            &state,
            state_root,
            None,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        if let Some(bridge) = &self.bridge {
            bridge.record_execution_summary_with_state_root(
                block.header().inner.number,
                block.hash(),
                execution_summary,
                block.header().inner.timestamp,
                block.header().inner.state_root,
            );
        }

        Ok(BlockBuilderOutcome {
            block_access_list: None,
            execution_result: result,
            hashed_state,
            trie_updates,
            block,
        })
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        &mut self.executor
    }

    fn executor(&self) -> &Self::Executor {
        &self.executor
    }

    fn into_executor(self) -> Self::Executor {
        self.executor
    }
}

#[cfg(test)]
mod tests;
