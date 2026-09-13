use crate::{builder::OutbeBlockBuilder, executor::OutbeBlockExecutor};

use alloy_evm::{
    eth::{EthBlockExecutionCtx, NextEvmEnvAttributes},
    Evm, EvmEnv,
};
use outbe_primitives::{OutbeBlock, OutbeExecutionData, OutbeHeader, OutbePrimitives};
use reth_ethereum::{
    chainspec::EthChainSpec,
    evm::{
        primitives::{Database, EvmEnvFor, ExecutionCtxFor, InspectorFor},
        revm::{db::State, primitives::hardfork::SpecId},
    },
    node::api::{ConfigureEngineEvm, ConfigureEvm, ExecutableTxIterator},
    TransactionSigned,
};
use reth_evm::{execute::BlockBuilder, EvmFor};
use reth_primitives_traits::{
    AlloyBlockHeader as _, Recovered, SealedBlock, SealedHeader, SignedTransaction as _,
};

use std::convert::Infallible;

use super::{
    system_tx_expectations_for_block, OutbeBlockAssembler, OutbeBlockExecutionCtx, OutbeEvmConfig,
    OutbeNextBlockEnvAttributes,
};

// ---------------------------------------------------------------------------
// ConfigureEvm
// ---------------------------------------------------------------------------

impl ConfigureEvm for OutbeEvmConfig {
    type Primitives = OutbePrimitives;
    type Error = Infallible;
    type NextBlockEnvCtx = OutbeNextBlockEnvAttributes;
    /// The block executor factory IS `OutbeEvmConfig` itself - it creates
    /// `OutbeBlockExecutor` instances.
    type BlockExecutorFactory = Self;
    type BlockAssembler = OutbeBlockAssembler;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &OutbeHeader) -> Result<EvmEnv<SpecId>, Self::Error> {
        Ok(EvmEnv::for_eth_block(
            header,
            self.inner.chain_spec(),
            self.inner.chain_spec().chain().id(),
            self.inner
                .chain_spec()
                .blob_params_at_timestamp(header.timestamp()),
        ))
    }

    fn next_evm_env(
        &self,
        parent: &OutbeHeader,
        attributes: &OutbeNextBlockEnvAttributes,
    ) -> Result<EvmEnv<SpecId>, Self::Error> {
        Ok(EvmEnv::for_eth_next_block(
            parent,
            NextEvmEnvAttributes {
                timestamp: attributes.inner.timestamp,
                suggested_fee_recipient: attributes.inner.suggested_fee_recipient,
                prev_randao: attributes.inner.prev_randao,
                gas_limit: attributes.inner.gas_limit,
                slot_number: attributes.inner.slot_number,
            },
            self.inner
                .chain_spec()
                .next_block_base_fee(parent, attributes.inner.timestamp)
                .unwrap_or_default(),
            self.inner.chain_spec(),
            self.inner.chain_spec().chain().id(),
            self.inner
                .chain_spec()
                .blob_params_at_timestamp(attributes.inner.timestamp),
        ))
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<OutbeBlock>,
    ) -> Result<OutbeBlockExecutionCtx<'a>, Self::Error> {
        let (
            expected_begin_system_txs,
            expected_end_system_txs,
            system_layout_error,
            recovered_proposer,
        ) = system_tx_expectations_for_block(block, self.ocomp_lifecycle_activation);

        Ok(OutbeBlockExecutionCtx {
            inner: EthBlockExecutionCtx {
                tx_count_hint: Some(block.body().transactions.len()),
                parent_hash: block.header().parent_hash(),
                parent_beacon_block_root: block.header().parent_beacon_block_root(),
                ommers: &[],
                withdrawals: block
                    .body()
                    .withdrawals
                    .as_ref()
                    .map(|w| std::borrow::Cow::Borrowed(w.as_slice())),
                extra_data: block.header().extra_data().clone(),
                slot_number: block.header().slot_number(),
            },
            timestamp_millis_part: block.header().timestamp_millis_part(),
            block_hash: Some(block.hash()),
            block_state_root: Some(block.header().state_root()),
            expected_begin_system_txs,
            expected_end_system_txs,
            system_layout_error,
            parent_consensus_metadata: None,
            proposer_evm_address: recovered_proposer,
            execute_outbe_block_hooks: true,
            // Validator path: body[0] arrives through `expected_begin_system_txs`.
            prebuilt_phase1_tx: None,
            // Validator path: the parent block is sealed and in MDBX by the
            // time the executor runs (validation happens after import), so
            // `sealed_header_by_hash` resolves the artifact via the provider
            // (lookup ladder step 2 in `RethAccountedParentArtifactProvider`).
            // The FCU-Valid -> MDBX-commit race is a proposer-side window only;
            // validators do not need the in-memory `parent_artifact_hint`
            // fallback here.
            parent_artifact_hint: None,
            // Validator path: a `TeeBootstrap` in the body is read via
            // `expected_begin_system_txs`, not injected here.
            pending_tee_bootstrap: None,
            execution_read_budget: None,
        })
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<OutbeHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<OutbeBlockExecutionCtx<'_>, Self::Error> {
        Ok(OutbeBlockExecutionCtx {
            inner: EthBlockExecutionCtx {
                tx_count_hint: None,
                parent_hash: parent.hash(),
                parent_beacon_block_root: attributes.inner.parent_beacon_block_root,
                ommers: &[],
                withdrawals: attributes
                    .inner
                    .withdrawals
                    .map(|w| std::borrow::Cow::Owned(w.into_inner())),
                extra_data: Self::sanitize_next_block_extra_data(attributes.inner.extra_data),
                slot_number: attributes.inner.slot_number,
            },
            timestamp_millis_part: attributes.timestamp_millis_part,
            block_hash: None,
            block_state_root: None,
            expected_begin_system_txs: Vec::new(),
            expected_end_system_txs: Vec::new(),
            system_layout_error: None,
            parent_consensus_metadata: attributes.parent_consensus_metadata,
            proposer_evm_address: attributes.proposer_evm_address,
            execute_outbe_block_hooks: attributes.execute_outbe_block_hooks,
            prebuilt_phase1_tx: attributes.prebuilt_phase1_tx,
            parent_artifact_hint: attributes.parent_artifact_hint,
            pending_tee_bootstrap: attributes.pending_tee_bootstrap,
            execution_read_budget: attributes.execution_read_budget,
        })
    }

    #[allow(refining_impl_trait)]
    fn create_block_builder<'a, DB, I>(
        &'a self,
        evm: EvmFor<Self, &'a mut State<DB>, I>,
        parent: &'a SealedHeader<OutbeHeader>,
        ctx: OutbeBlockExecutionCtx<'a>,
    ) -> impl BlockBuilder<
        Primitives = Self::Primitives,
        Executor = OutbeBlockExecutor<'a, EvmFor<Self, &'a mut State<DB>, I>>,
    >
    where
        DB: Database,
        I: InspectorFor<Self, &'a mut State<DB>> + 'a,
    {
        use alloy_evm::eth::EthBlockExecutor;

        let expected_begin_system_txs = ctx.expected_begin_system_txs.clone();
        let expected_end_system_txs = ctx.expected_end_system_txs.clone();
        let mut system_layout_error = ctx.system_layout_error.clone();
        let parent_consensus_metadata = ctx.parent_consensus_metadata.clone();
        let proposer_evm_address = ctx.proposer_evm_address;
        let execute_outbe_block_hooks = ctx.execute_outbe_block_hooks;
        let parent_hash = ctx.inner.parent_hash;
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

        OutbeBlockBuilder::new(
            OutbeBlockExecutor::new(
                EthBlockExecutor::new(
                    evm,
                    ctx.inner.clone(),
                    self.inner.chain_spec(),
                    self.inner.executor_factory.receipt_builder(),
                ),
                self.bridge.clone(),
                ctx.inner.extra_data.clone(),
                self.accounted_parent_artifact_provider.clone(),
                false,
                None,
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
            .with_compressed_entities_scope(compressed_entities_scope)
            .with_compressed_tree_service(self.compressed_tree_service.clone())
            .with_runtime_body_readers(runtime_body_readers, ctx.execution_read_budget.clone())
            .with_pending_tee_bootstrap(pending_tee_bootstrap)
            .with_ocomp_lifecycle_active(self.ocomp_lifecycle_active_at(block_number)),
            ctx,
            self.bridge.clone(),
            self.block_assembler(),
            parent,
        )
    }
}

// ---------------------------------------------------------------------------
// ConfigureEngineEvm
// ---------------------------------------------------------------------------

impl ConfigureEngineEvm<OutbeExecutionData> for OutbeEvmConfig {
    fn evm_env_for_payload(
        &self,
        payload: &OutbeExecutionData,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.evm_env(payload.block.header())
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a OutbeExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        let mut ctx = self.context_for_block(&payload.block)?;
        ctx.execution_read_budget = payload.execution_read_budget.clone();
        Ok(ctx)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &OutbeExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        let txs = payload.block.body().transactions.clone();
        let convert = |tx: TransactionSigned| {
            let signer = tx.try_recover()?;
            Ok::<Recovered<TransactionSigned>, alloy_consensus::crypto::RecoveryError>(
                Recovered::new_unchecked(tx, signer),
            )
        };

        Ok((txs, convert))
    }
}
