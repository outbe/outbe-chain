use std::sync::Arc;

use alloy_consensus::Transaction as _;
use alloy_primitives::U256;
use alloy_rlp::Encodable as _;
use either::Either;
use outbe_evm::{AccountedParentArtifact, OutbeEvmConfig, OutbeNextBlockEnvAttributes};
use outbe_primitives::{
    consensus::OUTBE_MAX_BLOCK_SIZE, reshare_artifact::decode_outbe_block_artifacts,
    OutbeBuiltPayload, OutbeHeader, OutbePayloadAttributes, OutbePrimitives, OutbeTxEnvelope,
};
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chainspec::{ChainSpec, ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE;
use reth_errors::{BlockExecutionError, BlockValidationError, ConsensusError};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutionOutput, BlockExecutor},
    ConfigureEvm, Evm, NextBlockEnvAttributes,
};
use reth_payload_builder::{BlobSidecars, EthBuiltPayload};
use reth_payload_primitives::{BuiltPayloadExecutedBlock, PayloadBuilderError};
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_primitives_traits::AlloyBlockHeader as _;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::{
    error::{Eip4844PoolTransactionError, InvalidPoolTransactionError},
    BestTransactions, BestTransactionsAttributes, PoolTransaction, TransactionPool,
    ValidPoolTransaction,
};
use revm::context_interface::Block as _;
use tracing::{debug, trace, warn};

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
            trie_handle,
            config,
            cancel,
            best_payload,
        } = args;
        let PayloadConfig {
            parent_header,
            attributes,
            payload_id,
        } = config;

        let state_provider = self.provider.state_by_block_hash(parent_header.hash())?;
        let state = StateProviderDatabase::new(state_provider.as_ref());
        let mut db = State::builder()
            .with_database(cached_reads.as_db_mut(state))
            .with_bundle_update()
            .build();

        let chain_spec = self.provider.chain_spec();
        let inner = attributes.inner();
        let block_number = parent_header.number().saturating_add(1);

        // / / prebuild and sign the Phase 1
        // (CertifiedParentAccounting) body[0] tx BEFORE the executor enters
        // `apply_pre_execution_changes`. The same `Recovered` is then handed
        // to `build_begin_system_txs` for body[0] so the pre-exec commit
        // witness and the body[0] tx are byte-identical (hash match). For
        // `block_number <= OutbeProtocolSchedule.genesis_bootstrap_block_number`
        // (greenfield: block 0 / block 1) the helper returns `None` and
        // Phase 1 is skipped entirely.
        let prebuilt_phase1_tx = self
            .evm_config
            .build_signed_phase1_tx(
                block_number,
                chain_spec.chain().id(),
                parent_header.hash(),
                attributes.parent_consensus_metadata().cloned(),
                attributes.proposer_evm_address(),
            )
            .map_err(|err| {
                warn!(target: "payload_builder", %err, "failed to prebuild Phase 1 system tx");
                PayloadBuilderError::Internal(err.into())
            })?;

        // decode the parent's accounted-parent artifact from
        // `parent_header.extra_data` so the executor has a fallback when the
        // [`AccountedParentArtifactProvider`] cannot see the parent (e.g.,
        // unfinalized side-chain whose header hasn't been indexed yet). The
        // executor validates this hint against the Phase 1 metadata's
        // `(finalized_block_number, finalized_block_hash)` before accepting it.
        let parent_artifact_hint =
            decode_outbe_block_artifacts(parent_header.extra_data().as_ref())
                .ok()
                .and_then(|artifacts| artifacts.execution_summary)
                .map(|summary| AccountedParentArtifact {
                    summary,
                    timestamp: parent_header.timestamp(),
                });

        // One-time TEE bootstrap: the consensus thread's TEE DKG coordination
        // stashes the assembled `TeeBootstrapPayload` in the bridge; the proposer
        // consumes it here and injects it into the begin-zone (slice 5.1). Only
        // the proposer's bridge is read; validators verify the body-carried
        // payload (slice 5.2). `take` so a single proposal carries it.
        //
        // Guard to block 1 (the fixed `committee_snapshot_block` target): every
        // node stashes its pending payload at startup, but only the block-1
        // proposer must inject it. Without this guard a node that did not propose
        // block 1 would still hold its pending payload and inject a stale
        // `TeeBootstrap` when it later proposes block N > 1 — which the executor
        // rejects (`committee_snapshot_block` mismatch / already bootstrapped),
        // stalling that slot.
        let pending_tee_bootstrap = if block_number == 1 {
            self.evm_config
                .bridge
                .as_ref()
                .and_then(|bridge| bridge.take_pending_tee_bootstrap())
        } else {
            None
        };

        let mut builder = self
            .evm_config
            .builder_for_next_block(
                &mut db,
                &parent_header,
                OutbeNextBlockEnvAttributes {
                    inner: NextBlockEnvAttributes {
                        timestamp: inner.timestamp,
                        suggested_fee_recipient: inner.suggested_fee_recipient,
                        prev_randao: inner.prev_randao,
                        gas_limit: self.builder_config.gas_limit(parent_header.gas_limit()),
                        parent_beacon_block_root: inner.parent_beacon_block_root,
                        withdrawals: inner.withdrawals.clone().map(Into::into),
                        extra_data: attributes.extra_data().clone(),
                        slot_number: inner.slot_number,
                    },
                    timestamp_millis_part: attributes.timestamp_millis_part(),
                    parent_consensus_metadata: attributes.parent_consensus_metadata().cloned(),
                    proposer_evm_address: attributes.proposer_evm_address(),
                    execute_outbe_block_hooks: true,
                    prebuilt_phase1_tx: prebuilt_phase1_tx.clone(),
                    parent_artifact_hint,
                    // Clone: the executor branch (expected begin-zone order /
                    // `block_has_tee_bootstrap`) needs the same payload the body
                    // builder injects below, so both deterministic paths agree.
                    pending_tee_bootstrap: pending_tee_bootstrap.clone(),
                },
            )
            .map_err(PayloadBuilderError::other)?;

        debug!(
            target: "payload_builder",
            id = %payload_id,
            parent_hash = ?parent_header.hash(),
            parent_number = parent_header.number(),
            timestamp_millis = attributes.timestamp_millis(),
            "building Outbe payload"
        );

        let mut cumulative_gas_used = 0u64;
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
        let mut total_fees = U256::ZERO;

        if let Some(ref handle) = trie_handle {
            builder
                .executor_mut()
                .set_state_hook(Some(Box::new(handle.state_hook())));
        }

        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        let mut blob_sidecars = BlobSidecars::Empty;
        let mut block_blob_count = 0u64;
        let mut block_transactions_rlp_length = 0usize;
        let blob_params = chain_spec.blob_params_at_timestamp(inner.timestamp);
        let protocol_max_blob_count = blob_params
            .as_ref()
            .map(|params| params.max_blob_count)
            .unwrap_or_default();
        let max_blob_count = self
            .builder_config
            .max_blobs_per_block
            .map(|user_limit| std::cmp::min(user_limit, protocol_max_blob_count).max(1))
            .unwrap_or(protocol_max_blob_count);
        let is_osaka = chain_spec.is_osaka_active_at_timestamp(inner.timestamp);
        let withdrawals_rlp_length = inner
            .withdrawals
            .as_ref()
            .map(|withdrawals| withdrawals.length())
            .unwrap_or(0);

        let begin_system_txs = self
            .evm_config
            .build_begin_system_txs(
                block_number,
                chain_spec.chain().id(),
                parent_header.hash(),
                attributes.extra_data(),
                attributes.parent_consensus_metadata().cloned(),
                attributes.proposer_evm_address(),
                // reuse the prebuilt body[0] tx
                // byte-for-byte. `build_begin_system_txs` validates
                // calldata + signer match before substitution.
                prebuilt_phase1_tx,
                // The same bootstrap payload the executor branch above received,
                // so the injected body matches the expected begin-zone order
                // (TeeBootstrap at begin_order 3, before OracleSlashWindow).
                pending_tee_bootstrap,
            )
            .map_err(|err| {
                warn!(target: "payload_builder", %err, "failed to build begin system transactions");
                PayloadBuilderError::Internal(err.into())
            })?;
        for tx in begin_system_txs {
            if cancel.is_cancelled() {
                return Ok(BuildOutcome::Cancelled);
            }
            let tx_rlp_len = tx.inner().length();
            let gas_used = builder
                .execute_transaction(tx)
                .map_err(|err| {
                    warn!(target: "payload_builder", %err, "failed to execute begin system transaction");
                    PayloadBuilderError::Internal(err.into())
                })?
                .tx_gas_used();
            block_transactions_rlp_length += tx_rlp_len;
            cumulative_gas_used = cumulative_gas_used.saturating_add(gas_used);
            trace!(
                target: "payload_builder",
                gas_used,
                "included begin system transaction"
            );
        }

        while let Some(pool_tx) = best_txs.next() {
            if cumulative_gas_used.saturating_add(pool_tx.gas_limit()) > block_gas_limit {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::ExceedsGasLimit(
                        pool_tx.gas_limit(),
                        block_gas_limit,
                    ),
                );
                continue;
            }

            if cancel.is_cancelled() {
                return Ok(BuildOutcome::Cancelled);
            }

            let tx = pool_tx.to_consensus();
            let tx_rlp_len = tx.inner().length();
            let estimated_block_size =
                block_transactions_rlp_length + tx_rlp_len + withdrawals_rlp_length + 1024;

            if is_osaka && estimated_block_size > MAX_RLP_BLOCK_SIZE {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::OversizedData {
                        size: estimated_block_size,
                        limit: MAX_RLP_BLOCK_SIZE,
                    },
                );
                continue;
            }

            // Outbe transport cap (always on, independent of the Osaka fork): a
            // block must fit one consensus P2P message. Skip txs that would push
            // the block over `OUTBE_MAX_BLOCK_SIZE` so the proposer never builds
            // a block validators would reject as undisseminable.
            if estimated_block_size > OUTBE_MAX_BLOCK_SIZE {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::OversizedData {
                        size: estimated_block_size,
                        limit: OUTBE_MAX_BLOCK_SIZE,
                    },
                );
                continue;
            }

            let mut blob_tx_sidecar = None;
            let tx_blob_count = tx.blob_count();
            if let Some(tx_blob_count) = tx_blob_count {
                if block_blob_count + tx_blob_count > max_blob_count {
                    best_txs.mark_invalid(
                        &pool_tx,
                        &InvalidPoolTransactionError::Eip4844(
                            Eip4844PoolTransactionError::TooManyEip4844Blobs {
                                have: block_blob_count + tx_blob_count,
                                permitted: max_blob_count,
                            },
                        ),
                    );
                    continue;
                }

                let sidecar = match self
                    .pool
                    .get_blob(*tx.hash())
                    .map_err(PayloadBuilderError::other)?
                {
                    Some(sidecar) if is_osaka && sidecar.is_eip7594() => Some(sidecar),
                    Some(sidecar) if !is_osaka && sidecar.is_eip4844() => Some(sidecar),
                    Some(sidecar) if is_osaka && !sidecar.is_eip7594() => {
                        best_txs.mark_invalid(
                            &pool_tx,
                            &InvalidPoolTransactionError::Eip4844(
                                Eip4844PoolTransactionError::UnexpectedEip4844SidecarAfterOsaka,
                            ),
                        );
                        trace!(target: "payload_builder", ?sidecar, "skipping unexpected pre-Osaka sidecar");
                        continue;
                    }
                    Some(_) => {
                        best_txs.mark_invalid(
                            &pool_tx,
                            &InvalidPoolTransactionError::Eip4844(
                                Eip4844PoolTransactionError::UnexpectedEip7594SidecarBeforeOsaka,
                            ),
                        );
                        continue;
                    }
                    None => {
                        best_txs.mark_invalid(
                            &pool_tx,
                            &InvalidPoolTransactionError::Eip4844(
                                Eip4844PoolTransactionError::MissingEip4844BlobSidecar,
                            ),
                        );
                        continue;
                    }
                };
                blob_tx_sidecar = sidecar;
            }

            let miner_fee = tx.effective_tip_per_gas(base_fee);
            let tx_hash = *tx.tx_hash();
            let gas_used = match builder.execute_transaction(tx) {
                Ok(gas_used) => gas_used.tx_gas_used(),
                Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                    error,
                    ..
                })) => {
                    if !error.is_nonce_too_low() {
                        best_txs.mark_invalid(
                            &pool_tx,
                            &InvalidPoolTransactionError::Consensus(
                                InvalidTransactionError::TxTypeNotSupported,
                            ),
                        );
                    }
                    trace!(target: "payload_builder", %error, ?tx_hash, "skipping invalid transaction");
                    continue;
                }
                Err(err) => return Err(PayloadBuilderError::evm(err)),
            };

            if let Some(blob_count) = tx_blob_count {
                block_blob_count += blob_count;
                if block_blob_count == max_blob_count {
                    best_txs.skip_blobs();
                }
            }

            block_transactions_rlp_length += tx_rlp_len;
            let miner_fee = miner_fee.unwrap_or_default();
            total_fees += U256::from(miner_fee) * U256::from(gas_used);
            cumulative_gas_used += gas_used;

            if let Some(sidecar) = blob_tx_sidecar {
                blob_sidecars.push_sidecar_variant(sidecar.as_ref().clone());
            }
        }

        if !is_better_payload(best_payload.as_ref(), total_fees) {
            drop(builder);
            return Ok(BuildOutcome::Aborted {
                fees: total_fees,
                cached_reads,
            });
        }

        let outcome = if let Some(mut handle) = trie_handle {
            builder.executor_mut().set_state_hook(None);
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

        let BlockBuilderOutcome {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        } = outcome;

        let requests = chain_spec
            .is_prague_active_at_timestamp(inner.timestamp)
            .then_some(execution_result.requests.clone());

        // capture the full execution result of the block we just built so
        // the proposer does NOT re-execute and re-root it at finalize time.
        // `builder` only borrowed `&mut db`, so after `finish` the merged
        // post-state bundle is back on our local `db` and can be taken here.
        // Reth's launch loop inserts `executed_block()` into the engine tree, so
        // `ExecutorActor`'s finalize-time `new_payload` becomes a cache hit
        // (validators already get this via their verify-time `new_payload`).
        // This is the SAME execution that produced the sealed block below, so the
        // cached state matches the sealed block hash exactly — no proposer/
        // validator divergence.
        let recovered_block = Arc::new(block);
        let execution_output = Arc::new(BlockExecutionOutput {
            state: db.take_bundle(),
            result: execution_result,
        });
        let executed_block = BuiltPayloadExecutedBlock::<OutbePrimitives> {
            recovered_block: recovered_block.clone(),
            execution_output,
            hashed_state: Either::Left(Arc::new(hashed_state)),
            trie_updates: Either::Left(Arc::new(trie_updates)),
        };

        let sealed_block = Arc::new(recovered_block.sealed_block().clone());

        if is_osaka && sealed_block.rlp_length() > MAX_RLP_BLOCK_SIZE {
            return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
                rlp_length: sealed_block.rlp_length(),
                max_rlp_length: MAX_RLP_BLOCK_SIZE,
            }));
        }

        // Outbe transport cap (always on): the sealed block must fit one
        // consensus P2P message. Final guard in case the per-tx estimate
        // undershot (e.g. system txs / extra_data added after selection).
        if sealed_block.rlp_length() > OUTBE_MAX_BLOCK_SIZE {
            return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
                rlp_length: sealed_block.rlp_length(),
                max_rlp_length: OUTBE_MAX_BLOCK_SIZE,
            }));
        }

        let inner =
            EthBuiltPayload::<OutbePrimitives>::new(sealed_block, total_fees, requests, None)
                .with_sidecars(blob_sidecars);
        let payload = OutbeBuiltPayload::new(inner, Some(executed_block));

        Ok(BuildOutcome::Better {
            payload,
            cached_reads,
        })
    }
}
