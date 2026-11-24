use crate::builder::OutbeBlockBuilder;
use crate::executor::{
    BlockExecutionDependencies, BlockExecutionIdentity, BlockExecutorInputs, BlockSystemPlan,
    OutbeBlockExecutor, ParentAccountingInputs,
};
use crate::factory::{OutbeEvm, OutbeEvmTypes};

use alloy_consensus::{BlockHeader as _, Header};
use alloy_eips::eip7840::BlobParams;
use alloy_primitives::{Bytes, U256};
use evm2::env::{BlockEnv, BlockEnvExt};
use outbe_primitives::{OutbeBlock, OutbeExecutionData, OutbeHeader, OutbePrimitives};
use reth_ethereum::{
    chainspec::EthChainSpec,
    evm::{
        spec_id_by_timestamp_and_block_number, EthBlockExecutionCtx, EthBlockExecutor, EthEvmEnv,
    },
    node::api::{ConfigureEngineEvm, ConfigureEvm, ExecutableTxIterator},
    TransactionSigned,
};
use reth_evm::{execute::BlockBuilder, EvmEnvFor, ExecutionCtxFor};
use reth_primitives_traits::{Recovered, SealedBlock, SealedHeader, SignedTransaction as _};
use std::{convert::Infallible, sync::Arc};

use super::{
    system_tx_expectations_for_block, OutbeBlockAssembler, OutbeBlockExecutionCtx, OutbeEvmConfig,
    OutbeNextBlockEnvAttributes,
};

fn block_env_with_blob_params<H: alloy_consensus::BlockHeader>(
    header: &H,
    blob_params: Option<BlobParams>,
) -> BlockEnv {
    let blob_basefee = header
        .excess_blob_gas()
        .zip(blob_params)
        .map(|(excess_blob_gas, params)| U256::from(params.calc_blob_fee(excess_blob_gas)))
        .unwrap_or_default();
    BlockEnvExt {
        number: U256::from(header.number()),
        beneficiary: header.beneficiary(),
        timestamp: U256::from(header.timestamp()),
        gas_limit: U256::from(header.gas_limit()),
        basefee: U256::from(header.base_fee_per_gas().unwrap_or_default()),
        difficulty: header.difficulty(),
        prevrandao: header
            .mix_hash()
            .map(|hash| U256::from_be_bytes(hash.0))
            .unwrap_or_default(),
        blob_basefee,
        slot_num: U256::from(header.slot_number().unwrap_or_default()),
        ext: (),
        _non_exhaustive: (),
    }
}

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

    fn evm_env(&self, header: &OutbeHeader) -> Result<EthEvmEnv<OutbeEvmTypes>, Self::Error> {
        let spec = spec_id_by_timestamp_and_block_number(
            self.chain_spec.as_ref(),
            header.timestamp(),
            header.number(),
        );
        let blob_params = self
            .chain_spec
            .as_ref()
            .blob_params_at_timestamp(header.timestamp());
        Ok(self.evm_env_from_base_spec(
            spec,
            block_env_with_blob_params(header, blob_params),
            blob_params,
        ))
    }

    fn next_evm_env(
        &self,
        parent: &OutbeHeader,
        attributes: &OutbeNextBlockEnvAttributes,
    ) -> Result<EthEvmEnv<OutbeEvmTypes>, Self::Error> {
        let base_fee = self
            .chain_spec
            .as_ref()
            .next_block_base_fee(parent, attributes.inner.timestamp)
            .unwrap_or_default();
        let blob_params = self
            .chain_spec
            .as_ref()
            .blob_params_at_timestamp(attributes.inner.timestamp);
        let header = Header {
            beneficiary: attributes.inner.suggested_fee_recipient,
            timestamp: attributes.inner.timestamp,
            number: parent.number() + 1,
            gas_limit: attributes.inner.gas_limit,
            base_fee_per_gas: Some(base_fee),
            mix_hash: attributes.inner.prev_randao,
            slot_number: attributes.inner.slot_number,
            excess_blob_gas: parent
                .inner
                .maybe_next_block_excess_blob_gas(blob_params)
                .or_else(|| blob_params.map(|_| 0)),
            ..Default::default()
        };
        let spec = spec_id_by_timestamp_and_block_number(
            self.chain_spec.as_ref(),
            header.timestamp,
            header.number,
        );
        Ok(self.evm_env_from_base_spec(
            spec,
            block_env_with_blob_params(&header, blob_params),
            blob_params,
        ))
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<OutbeBlock>,
    ) -> Result<OutbeBlockExecutionCtx<'a>, Self::Error>
    where
        Self: 'a,
    {
        let (expected_begin_system_txs, system_layout_error, recovered_proposer) =
            system_tx_expectations_for_block(block, &self.runtime_host.block_order());

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
            system_layout_error,
            parent_consensus_metadata: None,
            proposer_evm_address: recovered_proposer,
            execute_outbe_block_hooks: true,
            prebuilt_phase1_tx: None,
            parent_artifact_hint: None,
            pending_tee_bootstrap: None,
            execution_read_budget: None,
        })
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader<OutbeHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<OutbeBlockExecutionCtx<'_>, Self::Error> {
        let (extra_data, system_layout_error) =
            match Self::sanitize_next_block_extra_data(attributes.inner.extra_data) {
                Ok(extra_data) => (extra_data, None),
                Err(error) => (
                    Bytes::new(),
                    Some(crate::errors::SystemTxExpectationError::decode_block_artifacts(error)),
                ),
            };
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
                extra_data,
                slot_number: attributes.inner.slot_number,
            },
            timestamp_millis_part: attributes.timestamp_millis_part,
            block_hash: None,
            block_state_root: None,
            expected_begin_system_txs: Vec::new(),
            system_layout_error,
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
    fn create_block_builder<'a>(
        &'a self,
        mut evm: OutbeEvm<'a>,
        evm_env: EthEvmEnv<OutbeEvmTypes>,
        parent: &'a SealedHeader<OutbeHeader>,
        ctx: OutbeBlockExecutionCtx<'a>,
    ) -> impl BlockBuilder<Primitives = Self::Primitives, Executor = OutbeBlockExecutor<'a>>
    where
        Self: 'a,
    {
        evm.ext_mut().set_owned_by_executor(true);
        let withdrawals = ctx.inner.withdrawals.as_deref().map(<[_]>::to_vec);
        let mut inner_ctx = ctx.inner.clone();
        inner_ctx.withdrawals = None;

        let system_plan = BlockSystemPlan {
            expected_begin_system_txs: ctx.expected_begin_system_txs.clone(),
            system_layout_error: ctx.system_layout_error.clone(),
            proposer_evm_address: ctx.proposer_evm_address,
            execute_outbe_block_hooks: ctx.execute_outbe_block_hooks,
            prebuilt_phase1_tx: ctx.prebuilt_phase1_tx.clone(),
            pending_tee_bootstrap: ctx.pending_tee_bootstrap.clone(),
        };

        let inner = EthBlockExecutor::new(
            evm,
            inner_ctx,
            self.chain_spec.as_ref(),
            &self.receipt_builder,
        );
        let executor = OutbeBlockExecutor::new(
            inner,
            Arc::clone(&self.chain_spec),
            withdrawals,
            BlockExecutorInputs {
                identity: BlockExecutionIdentity {
                    block_extra_data: ctx.inner.extra_data.clone(),
                    validate_execution_summary: false,
                    block_hash: None,
                    block_state_root: None,
                    parent_hash: ctx.inner.parent_hash,
                },
                system_plan,
                parent_accounting: ParentAccountingInputs {
                    accounted_parent_artifact_provider: self
                        .accounted_parent_artifact_provider
                        .clone(),
                    parent_consensus_metadata: ctx.parent_consensus_metadata.clone(),
                    parent_artifact_hint: ctx.parent_artifact_hint,
                },
                dependencies: BlockExecutionDependencies {
                    bridge: self.bridge.clone(),
                    evm_signer: self.evm_signer.clone(),
                    runtime_host: Arc::clone(&self.runtime_host),
                },
            },
        );
        OutbeBlockBuilder::new(executor, evm_env, ctx, self.block_assembler(), parent)
            .with_bridge(self.bridge.clone())
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
