use alloy_eips::eip7928::{bal::DecodedBal, compute_block_access_list_hash_with_buf};
use alloy_primitives::B256;
use outbe_primitives::{
    consensus::ConsensusExecutionBridge,
    reshare_artifact::{decode_outbe_block_artifacts, encode_outbe_block_artifacts},
    OutbeHeader, OutbePrimitives,
};
use reth_ethereum::{evm::EthEvmEnv, trie::updates::TrieUpdates, TransactionSigned};
use reth_evm::execute::{
    BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, BlockExecutionError,
    BlockExecutionOutput, BlockExecutor, CommitChanges, ExecutorTx, GasOutput,
};
use reth_evm::RecoveredTx as _;
use reth_evm::StateUpdate;
use reth_primitives_traits::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::StateProvider;

use crate::{
    config::{OutbeBlockAssembler, OutbeBlockExecutionCtx},
    executor::OutbeBlockExecutor,
    factory::OutbeEvmTypes,
};

pub fn encode_final_header_artifacts(
    prefinal_extra_data: &[u8],
    execution_summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact,
    timestamp_millis_part: u64,
    enclave_witness: Option<alloy_primitives::Bytes>,
) -> Result<alloy_primitives::Bytes, BlockExecutionError> {
    let mut artifacts = decode_outbe_block_artifacts(prefinal_extra_data)
        .map_err(crate::errors::ExecutorValidationError::block_artifacts)?;
    artifacts.execution_summary = Some(execution_summary);
    artifacts.timestamp_millis_part = timestamp_millis_part;
    artifacts.enclave_witness = enclave_witness;
    encode_outbe_block_artifacts(&artifacts)
        .map_err(|err| crate::errors::ExecutorValidationError::block_artifacts(err).into())
}

pub struct OutbeBlockBuilder<'a> {
    pub executor: OutbeBlockExecutor<'a>,
    pub evm_env: EthEvmEnv<OutbeEvmTypes>,
    pub transactions: Vec<Recovered<TransactionSigned>>,
    pub ctx: OutbeBlockExecutionCtx<'a>,
    pub bridge: Option<ConsensusExecutionBridge>,
    pub parent: &'a SealedHeader<OutbeHeader>,
    pub assembler: &'a OutbeBlockAssembler,
}

impl<'a> OutbeBlockBuilder<'a> {
    pub fn new(
        executor: OutbeBlockExecutor<'a>,
        evm_env: EthEvmEnv<OutbeEvmTypes>,
        ctx: OutbeBlockExecutionCtx<'a>,
        assembler: &'a OutbeBlockAssembler,
        parent: &'a SealedHeader<OutbeHeader>,
    ) -> Self {
        Self {
            executor,
            evm_env,
            transactions: Vec::new(),
            ctx,
            bridge: None,
            parent,
            assembler,
        }
    }

    #[must_use]
    pub fn with_bridge(mut self, bridge: Option<ConsensusExecutionBridge>) -> Self {
        self.bridge = bridge;
        self
    }

    /// Encodes execution-produced header fields before state-root calculation.
    pub fn finalize_header_artifacts(&mut self) -> Result<(), BlockExecutionError> {
        let enclave_witness = self.executor.final_enclave_witness_bytes()?;
        self.ctx.inner.extra_data = encode_final_header_artifacts(
            self.ctx.inner.extra_data.as_ref(),
            self.executor.current_execution_summary(),
            self.ctx.timestamp_millis_part,
            enclave_witness,
        )?;
        self.executor
            .set_final_extra_data(self.ctx.inner.extra_data.clone());
        Ok(())
    }
}

impl<'a> BlockBuilder for OutbeBlockBuilder<'a> {
    type Primitives = OutbePrimitives;
    type Executor = OutbeBlockExecutor<'a>;
    type EvmEnv = EthEvmEnv<OutbeEvmTypes>;

    fn evm_env(&self) -> &Self::EvmEnv {
        &self.evm_env
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.executor.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&evm2::TxResult<OutbeEvmTypes>, evm2::TxStateChanges<'_>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let include_preexecuted_phase1_witness =
            self.executor.is_preexecuted_phase1_witness(tx.tx());

        if let Some(gas_used) = self
            .executor
            .execute_transaction_with_commit_condition((tx_env, &tx), f)?
        {
            self.transactions
                .push(Recovered::new_unchecked(tx.tx().clone(), *tx.signer()));
            Ok(Some(gas_used))
        } else if include_preexecuted_phase1_witness {
            self.transactions
                .push(Recovered::new_unchecked(tx.tx().clone(), *tx.signer()));
            Ok(None)
        } else {
            Ok(None)
        }
    }

    fn execute_transaction_with_result_closure(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&<Self::Executor as BlockExecutor>::TransactionResultWithState),
    ) -> Result<GasOutput, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        let output = self
            .executor
            .execute_transaction_without_commit((tx_env, &tx))?;
        f(&output);
        let gas_output = self.executor.commit_transaction(output)?;
        self.transactions
            .push(Recovered::new_unchecked(tx.tx().clone(), *tx.signer()));
        Ok(gas_output)
    }

    fn set_state_hook(&mut self, hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        self.executor.set_state_hook(hook)
    }

    fn finish_with_state_root(
        mut self,
        state_provider: impl StateProvider,
        state_root: impl FnOnce(
            &BlockExecutionOutput<reth_ethereum::Receipt>,
        ) -> Result<Option<(B256, TrieUpdates)>, BlockExecutionError>,
    ) -> Result<BlockBuilderOutcome<OutbePrimitives>, BlockExecutionError> {
        // Sub-second timestamp travels in `extra_data` under tag 0x05 so
        // the block hash stays Ethereum-spec-compliant
        // (`keccak256(rlp(standard_header))`).
        let execution_summary = self.executor.current_execution_summary();
        self.transactions
            .extend(self.executor.take_staged_end_block_txs());
        self.finalize_header_artifacts()?;
        let (output, block_access_list) = self.executor.finish_with_block_access_list()?;

        let block_access_list = block_access_list.map(|bal| {
            let mut raw = Vec::new();
            let hash = compute_block_access_list_hash_with_buf(&bal, &mut raw);
            DecodedBal::new_unchecked(bal.into(), raw.into(), hash)
        });
        let block_access_list_hash = block_access_list.as_ref().map(DecodedBal::hash);

        let hashed_state = state_provider
            .hashed_post_state(&output.state)
            .map_err(BlockExecutionError::other)?;
        let (state_root, trie_updates) = match state_root(&output)? {
            Some(precomputed) => precomputed,
            None => state_provider
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
            self.evm_env,
            self.ctx,
            self.parent,
            transactions,
            &output.result,
            &output.state,
            &state_provider,
            state_root,
            block_access_list_hash,
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
            execution_result: output.result,
            execution_state: output.state,
            hashed_state,
            trie_updates,
            block,
            block_access_list,
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
