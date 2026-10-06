use std::sync::Arc;

use alloy_primitives::B256;
use outbe_evm::OutbeEvmConfig;
use outbe_primitives::{
    error::PrecompileError,
    runtime_audit_v1::{BODY_READ_REQUEST_DEADLINE, OTHER_PAYLOAD_EXECUTION_FAILURE},
    system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
    OutbeBuiltPayload, OutbeHeader, OutbePayloadAttributes, OutbeTxEnvelope,
};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chainspec::{ChainSpec, ChainSpecProvider, EthChainSpec};
use reth_errors::BlockExecutionError;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{execute::BlockBuilder, ConfigureEvm, Evm};
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::AlloyBlockHeader as _;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::{
    BestTransactions, BestTransactionsAttributes, PoolTransaction, TransactionPool,
    ValidPoolTransaction,
};
use revm::context_interface::Block as _;
use tracing::{debug, warn};

mod carrier_admission;
mod execution;
mod finalization;
mod preparation;
mod selection;
mod size_budget;

use carrier_admission::CarrierBlock;

#[derive(Debug, Clone)]
pub struct OutbePayloadBuilder<Pool, Provider> {
    pool: Pool,
    provider: Provider,
    evm_config: OutbeEvmConfig,
    builder_config: EthereumBuilderConfig,
}

impl<Pool, Provider> OutbePayloadBuilder<Pool, Provider> {
    pub const fn new(
        provider: Provider,
        pool: Pool,
        evm_config: OutbeEvmConfig,
        builder_config: EthereumBuilderConfig,
    ) -> Self {
        Self {
            pool,
            provider,
            evm_config,
            builder_config,
        }
    }
}

impl<Pool, Provider> PayloadBuilder for OutbePayloadBuilder<Pool, Provider>
where
    Provider: StateProviderFactory + ChainSpecProvider<ChainSpec = ChainSpec<OutbeHeader>> + Clone,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = OutbeTxEnvelope>>,
{
    type Attributes = OutbePayloadAttributes;
    type BuiltPayload = OutbeBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        self.build_payload(args, |attrs| {
            self.pool.best_transactions_with_attributes(attrs)
        })
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, OutbeHeader>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        self.build_payload(
            BuildArguments::new(
                Default::default(),
                Default::default(),
                None,
                config,
                Default::default(),
                None,
            ),
            |_| core::iter::empty(),
        )?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

impl<Pool, Provider> OutbePayloadBuilder<Pool, Provider>
where
    Provider: StateProviderFactory + ChainSpecProvider<ChainSpec = ChainSpec<OutbeHeader>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = OutbeTxEnvelope>>,
{
    fn build_payload<Txs>(
        &self,
        args: BuildArguments<OutbePayloadAttributes, OutbeBuiltPayload>,
        best_txs: impl FnOnce(BestTransactionsAttributes) -> Txs,
    ) -> Result<BuildOutcome<OutbeBuiltPayload>, PayloadBuilderError>
    where
        Txs: BestTransactions<Item = Arc<ValidPoolTransaction<Pool::Transaction>>>,
    {
        let BuildArguments {
            mut cached_reads,
            execution_cache: _,
            mut state_root_handle,
            config,
            cancel,
            best_payload,
        } = args;
        let PayloadConfig {
            parent_header,
            attributes,
            payload_id,
            parent_block_info: _,
        } = config;

        let state_provider = self.provider.state_by_block_hash(parent_header.hash())?;
        let state = StateProviderDatabase::new(state_provider.as_ref());
        let mut db = State::builder()
            .with_database(cached_reads.as_db_mut(state))
            .with_bundle_update()
            .build();

        let chain_spec = self.provider.chain_spec();
        let block_number = parent_header.number().saturating_add(1);
        let context = preparation::PayloadContext {
            parent: &parent_header,
            attributes: &attributes,
            chain_spec: &chain_spec,
        };
        let preparation::PreparedPayload { env, system_inputs } =
            preparation::prepare(&self.evm_config, &context)?;
        let mut builder = self
            .evm_config
            .builder_for_next_block(&mut db, &parent_header, env)
            .map_err(PayloadBuilderError::other)?;
        let compressed_tree_service = self.evm_config.compressed_tree_service();
        debug!(
            target: "payload_builder",
            id = %payload_id,
            parent_hash = ?parent_header.hash(),
            parent_number = parent_header.number(),
            timestamp_millis = attributes.timestamp_millis(),
            "building Outbe payload"
        );

        let block_gas_limit = builder.evm_mut().block().gas_limit();
        let base_fee = builder.evm_mut().block().basefee();
        let mut best_txs = best_txs(BestTransactionsAttributes::new(
            base_fee,
            builder
                .evm_mut()
                .block()
                .blob_gasprice()
                .map(|gasprice| gasprice as u64),
        ));
        if let Some(handle) = state_root_handle.as_mut() {
            builder
                .evm_mut()
                .db_mut()
                .set_state_hook(Some(Box::new(handle.take_state_hook())));
        }

        preparation::apply_pre_execution_changes(&mut builder)?;
        let system_txs = preparation::SystemTransactions::build(
            &self.evm_config,
            &context,
            block_gas_limit,
            system_inputs,
        )?;
        let mut state = execution::PayloadBuildState::new(
            &context,
            &self.builder_config,
            block_gas_limit,
            base_fee,
            &system_txs.end,
        )?;
        if state.execute_begin(&mut builder, system_txs.begin, &cancel)?
            == execution::StageOutcome::Cancelled
        {
            return Ok(BuildOutcome::Cancelled);
        }
        // The builder checks result-vote carriers on the exact in-progress block
        // state before execution. See `carrier_admission`.
        let carrier_block = self
            .evm_config
            .ocomp_lifecycle_active_at(block_number)
            .then(|| {
                let block = builder.evm_mut().block();
                CarrierBlock {
                    number: block_number,
                    timestamp: block.timestamp().saturating_to::<u64>(),
                    chain_id: chain_spec.chain().id(),
                    genesis_hash: chain_spec.genesis_hash(),
                    beneficiary: block.beneficiary(),
                }
            });

        if block_number != GENESIS_BOOTSTRAP_BLOCK_NUMBER {
            let mut selection = selection::UserTransactions {
                pool: &self.pool,
                best_txs: &mut best_txs,
                state: &mut state,
                cancel: &cancel,
                payload_id,
                carrier_block,
            };
            if selection.execute(&mut builder)? == execution::StageOutcome::Cancelled {
                return Ok(BuildOutcome::Cancelled);
            }
        }
        if !is_better_payload(best_payload.as_ref(), state.total_fees) {
            drop(builder);
            return Ok(BuildOutcome::Aborted {
                fees: state.total_fees,
                cached_reads,
            });
        }
        if state.execute_end(&mut builder, system_txs.end, &cancel)?
            == execution::StageOutcome::Cancelled
        {
            return Ok(BuildOutcome::Cancelled);
        }
        let outcome = if let Some(mut handle) = state_root_handle {
            // CE end-block cleanup is consensus state. Deliver its zeroing
            // changes to the parallel trie task before detaching the hook and
            // freezing the precomputed root.
            if !self.evm_config.ocomp_lifecycle_active_at(block_number) {
                builder
                    .executor_mut()
                    .finalize_compressed_entities()
                    .map_err(PayloadBuilderError::evm)?;
            }
            builder
                .executor_mut()
                .prepare_final_header_artifacts(attributes.timestamp_millis_part())
                .map_err(PayloadBuilderError::evm)?;
            builder.evm_mut().db_mut().set_state_hook(None);
            match handle.state_root() {
                Ok(outcome) => builder.finish(
                    state_provider.as_ref(),
                    Some((
                        outcome.state_root,
                        Arc::unwrap_or_clone(outcome.trie_updates),
                    )),
                )?,
                Err(err) => {
                    warn!(target: "payload_builder", id=%payload_id, %err, "sparse trie failed, falling back to sync state root");
                    builder.finish(state_provider.as_ref(), None)?
                }
            }
        } else {
            builder.finish(state_provider.as_ref(), None)?
        };

        let payload = finalization::into_payload(
            outcome,
            &mut db,
            state,
            &context,
            compressed_tree_service.as_ref(),
        )?;
        Ok(BuildOutcome::Better {
            payload,
            cached_reads,
        })
    }
}

fn discard_failed_payload_candidate(
    service: Option<&Arc<outbe_compressed_entities::CompressedTreeService>>,
    block_number: u64,
    block_hash: B256,
) -> Result<(), PayloadBuilderError> {
    if let Some(service) = service {
        service
            .discard_candidate(block_number, block_hash)
            .map_err(PayloadBuilderError::other)?;
    }
    Ok(())
}

fn ce_work_admission_error(error: &BlockExecutionError) -> Option<&PrecompileError> {
    error.as_internal()?.downcast_other::<PrecompileError>()
}

fn payload_execution_failure_kind(error: &BlockExecutionError) -> &'static str {
    if matches!(
        ce_work_admission_error(error),
        Some(PrecompileError::BodyReadRequestDeadline)
    ) {
        BODY_READ_REQUEST_DEADLINE
    } else {
        OTHER_PAYLOAD_EXECUTION_FAILURE
    }
}

fn ce_local_readiness_error(error: &BlockExecutionError) -> bool {
    matches!(
        ce_work_admission_error(error),
        Some(PrecompileError::TreeUnavailable(_))
    )
}

#[cfg(test)]
mod tests;
