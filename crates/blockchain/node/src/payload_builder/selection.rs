//! Ordered admission and execution of pool transactions.

use std::sync::Arc;

use alloy_consensus::Transaction as _;
use alloy_eips::eip7594::BlobTransactionSidecarVariant;
use alloy_rlp::Encodable as _;
use alloy_rpc_types_engine::PayloadId;
use outbe_primitives::{
    error::PrecompileError,
    runtime_audit_v1::{process_instance_id, PAYLOAD_EXECUTION_FAILED, SCHEMA_VERSION},
    OutbePrimitives, OutbeTxEnvelope,
};
use reth_errors::{BlockExecutionError, BlockValidationError};
use reth_evm::{
    block::StateDB,
    execute::{BlockBuilder, BlockExecutor},
    Evm,
};
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_primitives_traits::Recovered;
use reth_revm::cancelled::CancelOnDrop;
use reth_transaction_pool::{
    error::{Eip4844PoolTransactionError, InvalidPoolTransactionError},
    BestTransactions, PoolTransaction, TransactionPool, ValidPoolTransaction,
};
use tracing::{debug, trace, warn};

use super::{
    carrier_admission::{
        self, CarrierBlock, CarrierDecision, DeferredResultVoteCarrier, InvalidResultVoteCarrier,
    },
    ce_work_admission_error,
    execution::{PayloadBuildState, StageOutcome},
    payload_execution_failure_kind,
    size_budget::SizeRejection,
};

enum BlobAdmission {
    Accepted(Option<Arc<BlobTransactionSidecarVariant>>),
    Rejected,
}

/// Values captured before consuming the signed transaction in execution.
struct UserAccounting {
    rlp_length: usize,
    miner_fee: Option<u128>,
    blob_count: Option<u64>,
}

/// One iterator's admission policy and the mutable accounting for its attempt.
pub(super) struct UserTransactions<'a, Pool, Txs> {
    pub(super) pool: &'a Pool,
    pub(super) best_txs: &'a mut Txs,
    pub(super) state: &'a mut PayloadBuildState,
    pub(super) cancel: &'a CancelOnDrop,
    pub(super) payload_id: PayloadId,
    pub(super) carrier_block: Option<CarrierBlock>,
}

impl<Pool, Txs> UserTransactions<'_, Pool, Txs>
where
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = OutbeTxEnvelope>>,
    Txs: BestTransactions<Item = Arc<ValidPoolTransaction<Pool::Transaction>>>,
{
    pub(super) fn execute<B>(
        &mut self,
        builder: &mut B,
    ) -> Result<StageOutcome, PayloadBuilderError>
    where
        B: BlockBuilder<Primitives = OutbePrimitives>,
        <<B::Executor as BlockExecutor>::Evm as Evm>::DB: StateDB,
    {
        while let Some(pool_tx) = self.best_txs.next() {
            // Preserve gas-before-cancel ordering: a gas-rejected candidate
            // is skipped even if the cancellation signal has already fired.
            if !self.admit_gas(&pool_tx) {
                continue;
            }
            if self.cancel.is_cancelled() {
                return Ok(StageOutcome::Cancelled);
            }
            let tx = pool_tx.to_consensus();
            let tx_rlp_len = tx.inner().length();
            if !self.admit_size(&pool_tx, tx_rlp_len) {
                continue;
            }
            let sidecar = match self.blob_sidecar(&pool_tx, &tx)? {
                BlobAdmission::Accepted(sidecar) => sidecar,
                BlobAdmission::Rejected => continue,
            };
            if !self.admit_carrier(builder.evm_mut().db_mut(), &pool_tx, &tx)? {
                continue;
            }
            let accounting = UserAccounting {
                rlp_length: tx_rlp_len,
                miner_fee: tx.effective_tip_per_gas(self.state.base_fee),
                blob_count: tx.blob_count(),
            };
            let Some(gas_used) = self.execute_transaction(builder, &pool_tx, tx)? else {
                continue;
            };
            self.record_included(accounting, gas_used, sidecar);
        }
        Ok(StageOutcome::Completed)
    }

    fn record_included(
        &mut self,
        accounting: UserAccounting,
        gas_used: u64,
        sidecar: Option<Arc<BlobTransactionSidecarVariant>>,
    ) {
        if let Some(blob_count) = accounting.blob_count {
            self.state.block_blob_count += blob_count;
            if self.state.block_blob_count == self.state.max_blob_count {
                self.best_txs.skip_blobs();
            }
        }
        self.state
            .record_user(accounting.rlp_length, accounting.miner_fee, gas_used);
        if let Some(sidecar) = sidecar {
            self.state
                .blob_sidecars
                .push_sidecar_variant(sidecar.as_ref().clone());
        }
    }

    fn admit_gas(&mut self, pool_tx: &Arc<ValidPoolTransaction<Pool::Transaction>>) -> bool {
        if self
            .state
            .cumulative_gas_used
            .saturating_add(pool_tx.gas_limit())
            .saturating_add(self.state.reserved_end_gas)
            > self.state.block_gas_limit
        {
            self.best_txs.mark_invalid(
                pool_tx,
                InvalidPoolTransactionError::ExceedsGasLimit(
                    pool_tx.gas_limit(),
                    self.state.block_gas_limit,
                ),
            );
            return false;
        }

        true
    }

    fn admit_size(
        &mut self,
        pool_tx: &Arc<ValidPoolTransaction<Pool::Transaction>>,
        tx_rlp_len: usize,
    ) -> bool {
        if let Err(SizeRejection { size, limit }) = self.state.size_budget.admit(tx_rlp_len) {
            self.best_txs.mark_invalid(
                pool_tx,
                InvalidPoolTransactionError::OversizedData { size, limit },
            );
            return false;
        }

        true
    }

    fn blob_sidecar(
        &mut self,
        pool_tx: &Arc<ValidPoolTransaction<Pool::Transaction>>,
        tx: &Recovered<OutbeTxEnvelope>,
    ) -> Result<BlobAdmission, PayloadBuilderError> {
        let mut blob_tx_sidecar = None;
        let tx_blob_count = tx.blob_count();
        if let Some(tx_blob_count) = tx_blob_count {
            if self.state.block_blob_count + tx_blob_count > self.state.max_blob_count {
                self.best_txs.mark_invalid(
                    pool_tx,
                    InvalidPoolTransactionError::Eip4844(
                        Eip4844PoolTransactionError::TooManyEip4844Blobs {
                            have: self.state.block_blob_count + tx_blob_count,
                            permitted: self.state.max_blob_count,
                        },
                    ),
                );
                return Ok(BlobAdmission::Rejected);
            }

            let sidecar = match self
                .pool
                .get_blob(*tx.hash())
                .map_err(PayloadBuilderError::other)?
            {
                Some(sidecar) if self.state.is_osaka && sidecar.is_eip7594() => Some(sidecar),
                Some(sidecar) if !self.state.is_osaka && sidecar.is_eip4844() => Some(sidecar),
                Some(sidecar) if self.state.is_osaka && !sidecar.is_eip7594() => {
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Eip4844(
                            Eip4844PoolTransactionError::UnexpectedEip4844SidecarAfterOsaka,
                        ),
                    );
                    trace!(target: "payload_builder", ?sidecar, "skipping unexpected pre-Osaka sidecar");
                    return Ok(BlobAdmission::Rejected);
                }
                Some(_) => {
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Eip4844(
                            Eip4844PoolTransactionError::UnexpectedEip7594SidecarBeforeOsaka,
                        ),
                    );
                    return Ok(BlobAdmission::Rejected);
                }
                None => {
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Eip4844(
                            Eip4844PoolTransactionError::MissingEip4844BlobSidecar,
                        ),
                    );
                    return Ok(BlobAdmission::Rejected);
                }
            };
            blob_tx_sidecar = sidecar;
        }

        Ok(BlobAdmission::Accepted(blob_tx_sidecar))
    }

    fn admit_carrier<DB: StateDB>(
        &mut self,
        db: &mut DB,
        pool_tx: &Arc<ValidPoolTransaction<Pool::Transaction>>,
        tx: &Recovered<OutbeTxEnvelope>,
    ) -> Result<bool, PayloadBuilderError> {
        if let Some(block) = self.carrier_block {
            match carrier_admission::admit(db, block, tx.inner(), tx.signer()) {
                CarrierDecision::Execute => {}
                CarrierDecision::Skip => {
                    debug!(
                        target: "payload_builder",
                        payload_id = %self.payload_id,
                        tx_hash = ?tx.tx_hash(),
                        "skipping result-vote carrier that is invalid on this block state"
                    );
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Other(Box::new(InvalidResultVoteCarrier)),
                    );
                    return Ok(false);
                }
                CarrierDecision::Defer => {
                    debug!(
                        target: "payload_builder",
                        payload_id = %self.payload_id,
                        tx_hash = ?tx.tx_hash(),
                        "deferring result-vote carrier whose due window is not closed yet"
                    );
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Other(Box::new(DeferredResultVoteCarrier)),
                    );
                    return Ok(false);
                }
                CarrierDecision::Abort(reason) => {
                    warn!(
                        target: "payload_builder",
                        payload_id = %self.payload_id,
                        tx_hash = ?tx.tx_hash(),
                        %reason,
                        "result-vote carrier check cannot decide; abandoning this build"
                    );
                    return Err(PayloadBuilderError::other(reason));
                }
            }
        }

        Ok(true)
    }

    fn execute_transaction<B: BlockBuilder<Primitives = OutbePrimitives>>(
        &mut self,
        builder: &mut B,
        pool_tx: &Arc<ValidPoolTransaction<Pool::Transaction>>,
        tx: Recovered<OutbeTxEnvelope>,
    ) -> Result<Option<u64>, PayloadBuilderError> {
        let tx_hash = *tx.tx_hash();
        match builder.execute_transaction(tx) {
            Ok(gas_used) => Ok(Some(gas_used.tx_gas_used())),
            Err(err)
                if matches!(
                    ce_work_admission_error(&err),
                    Some(PrecompileError::BlockCeWorkCapacityExhausted)
                ) =>
            {
                trace!(target: "payload_builder", ?tx_hash, "deferring transaction because the payload CE work budget is exhausted");
                Ok(None)
            }
            Err(err)
                if matches!(
                    ce_work_admission_error(&err),
                    Some(PrecompileError::TransactionCeWorkLimitExceeded)
                ) =>
            {
                trace!(target: "payload_builder", ?tx_hash, "skipping transaction that cannot fit the full CE work limit");
                Ok(None)
            }
            Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                error, ..
            })) => {
                if !error.is_nonce_too_low() {
                    self.best_txs.mark_invalid(
                        pool_tx,
                        InvalidPoolTransactionError::Consensus(
                            InvalidTransactionError::TxTypeNotSupported,
                        ),
                    );
                }
                trace!(target: "payload_builder", %error, ?tx_hash, "skipping invalid transaction");
                Ok(None)
            }
            Err(err) => {
                let failure_kind = payload_execution_failure_kind(&err);
                debug!(
                    target: "payload_builder",
                    audit_schema = SCHEMA_VERSION,
                    audit_event = %PAYLOAD_EXECUTION_FAILED,
                    process_instance = %process_instance_id(),
                    payload_id = %self.payload_id,
                    ?tx_hash,
                    failure_kind = %failure_kind,
                    %err,
                    "payload transaction execution failed"
                );
                Err(PayloadBuilderError::evm(err))
            }
        }
    }
}
