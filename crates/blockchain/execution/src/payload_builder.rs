use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use outbe_evm::OutbeEvmConfig;
use outbe_primitives::{
    block::BlockHeightRegime, runtime_audit_v1::OTHER_PAYLOAD_EXECUTION_FAILURE, OutbeBuiltPayload,
    OutbeHeader, OutbePayloadAttributes, OutbePrimitives, OutbeTxEnvelope,
};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chainspec::{ChainSpec, ChainSpecProvider};
use reth_errors::BlockExecutionError;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm,
};
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::AlloyBlockHeader as _;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::{StateProvider, StateProviderFactory};
use reth_transaction_pool::{
    BestTransactions, BestTransactionsAttributes, PoolTransaction, TransactionPool,
    ValidPoolTransaction,
};
use revm::context_interface::Block as _;
use tracing::{debug, warn};

mod execution;
mod finalization;
mod preparation;
mod selection;
mod size_budget;
mod transaction_error;

pub use selection::PayloadTxSkipReason;

/// Closed terminal outcomes for this payload builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadBuildStopReason {
    Cancelled,
    CandidatesExhausted,
    AbortedNotBetter,
}

impl PayloadBuildStopReason {
    pub const fn as_label(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::CandidatesExhausted => "candidates_exhausted",
            Self::AbortedNotBetter => "aborted_not_better",
        }
    }
}

/// Record one terminal outcome; errors and unsupported Freeze outcomes emit nothing.
/// Outbe currently returns only Better, Aborted, or Cancelled.
pub fn record_payload_build_outcome<T>(
    outcome: &Result<BuildOutcome<T>, PayloadBuilderError>,
    elapsed: Duration,
) {
    let reason = match outcome {
        Ok(BuildOutcome::Better { .. }) => {
            metrics::histogram!("outbe_evm_payload_build_duration_ms")
                .record(elapsed.as_secs_f64() * 1000.0);
            PayloadBuildStopReason::CandidatesExhausted
        }
        Ok(BuildOutcome::Aborted { .. }) => PayloadBuildStopReason::AbortedNotBetter,
        Ok(BuildOutcome::Cancelled) => PayloadBuildStopReason::Cancelled,
        Ok(BuildOutcome::Freeze(_)) | Err(_) => return,
    };
    metrics::counter!("outbe_evm_payload_build_stop_total", "reason" => reason.as_label())
        .increment(1);
}

fn observe_payload_build<P>(
    f: impl FnOnce() -> Result<BuildOutcome<P>, PayloadBuilderError>,
) -> Result<BuildOutcome<P>, PayloadBuilderError> {
    let started = Instant::now();
    let outcome = f();
    record_payload_build_outcome(&outcome, started.elapsed());
    outcome
}

/// Record the complete root calculation and builder finish phase in milliseconds.
pub fn record_payload_state_root_duration(elapsed: Duration) {
    metrics::histogram!("outbe_evm_payload_state_root_duration_ms")
        .record(elapsed.as_secs_f64() * 1000.0);
}

/// Record a failed parallel root calculation before the synchronous fallback.
pub fn record_payload_sparse_trie_fallback() {
    metrics::counter!("outbe_evm_payload_sparse_trie_fallback_total").increment(1);
}

/// Record the exact size of a successfully finalized sealed block.
pub fn record_payload_rlp_size(rlp_bytes: usize) {
    metrics::gauge!("outbe_evm_payload_rlp_size_bytes").set(rlp_bytes as f64);
}

fn measure_payload_state_root<T>(
    finish: impl FnOnce() -> Result<T, PayloadBuilderError>,
) -> Result<T, PayloadBuilderError> {
    let started = Instant::now();
    let outcome = finish();
    if outcome.is_ok() {
        record_payload_state_root_duration(started.elapsed());
    }
    outcome
}

type PayloadStateRoot = (alloy_primitives::B256, reth_trie::updates::TrieUpdates);

/// Keep preparation errors fatal, but recover a failed parallel root synchronously.
fn finish_payload_with_state_root<B, P, F, E>(
    mut builder: B,
    state_provider: P,
    state_root: F,
    payload_id: alloy_rpc_types_engine::PayloadId,
) -> Result<BlockBuilderOutcome<OutbePrimitives>, PayloadBuilderError>
where
    B: BlockBuilder<Primitives = OutbePrimitives>,
    P: StateProvider,
    F: FnOnce(&mut B) -> Result<Option<Result<PayloadStateRoot, E>>, PayloadBuilderError>,
    E: std::fmt::Display,
{
    measure_payload_state_root(|| {
        let state_root = match state_root(&mut builder)? {
            Some(Ok(root)) => Some(root),
            Some(Err(err)) => {
                warn!(target: "payload_builder", id=%payload_id, %err, "sparse trie failed, falling back to sync state root");
                record_payload_sparse_trie_fallback();
                None
            }
            None => None,
        };
        builder
            .finish(state_provider, state_root)
            .map_err(PayloadBuilderError::evm)
    })
}

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
    pub fn build_payload<Txs>(
        &self,
        args: BuildArguments<OutbePayloadAttributes, OutbeBuiltPayload>,
        best_txs: impl FnOnce(BestTransactionsAttributes) -> Txs,
    ) -> Result<BuildOutcome<OutbeBuiltPayload>, PayloadBuilderError>
    where
        Txs: BestTransactions<Item = Arc<ValidPoolTransaction<Pool::Transaction>>>,
    {
        observe_payload_build(|| self.build_payload_inner(args, best_txs))
    }

    fn build_payload_inner<Txs>(
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
        let state =
            StateProviderDatabase::new(reth_storage_api::EvmStateProviderAdapter(&state_provider));
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
        )?;
        if state.execute_begin(&mut builder, system_txs.begin, &cancel)?
            == execution::StageOutcome::Cancelled
        {
            return Ok(BuildOutcome::Cancelled);
        }
        if BlockHeightRegime::for_block(block_number).allows_user_transactions() {
            let mut selection = selection::UserTransactions {
                pool: &self.pool,
                best_txs: &mut best_txs,
                state: &mut state,
                cancel: &cancel,
                payload_id,
            };
            if selection.execute(&mut builder)? == execution::StageOutcome::Cancelled {
                return Ok(BuildOutcome::Cancelled);
            }
        }
        builder
            .executor_mut()
            .apply_end_block_phase()
            .map_err(PayloadBuilderError::evm)?;
        if !is_better_payload(best_payload.as_ref(), state.total_fees) {
            drop(builder);
            return Ok(BuildOutcome::Aborted {
                fees: state.total_fees,
                cached_reads,
            });
        }
        let outcome = finish_payload_with_state_root(
            builder,
            state_provider.as_ref(),
            |builder| {
                let Some(mut handle) = state_root_handle else {
                    return Ok(None);
                };
                // Deliver all EVM state changes before the parallel root is frozen.
                builder
                    .executor_mut()
                    .prepare_final_header_artifacts(attributes.timestamp_millis_part())
                    .map_err(PayloadBuilderError::evm)?;
                builder.evm_mut().db_mut().set_state_hook(None);
                Ok(Some(handle.state_root().map(|outcome| {
                    (
                        outcome.state_root,
                        Arc::unwrap_or_clone(outcome.trie_updates),
                    )
                })))
            },
            payload_id,
        )?;

        let payload = finalization::into_payload(outcome, &mut db, state, &context)?;
        Ok(BuildOutcome::Better {
            payload,
            cached_reads,
        })
    }
}

fn payload_execution_failure_kind(_error: &BlockExecutionError) -> &'static str {
    OTHER_PAYLOAD_EXECUTION_FAILURE
}

#[cfg(test)]
mod tests;
