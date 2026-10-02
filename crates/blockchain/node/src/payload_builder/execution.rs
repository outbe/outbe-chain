//! Per-attempt budgets and system-transaction execution.

use alloy_consensus::Transaction as _;
use alloy_primitives::U256;
use alloy_rlp::Encodable as _;
use outbe_primitives::{OutbePrimitives, OutbeTxEnvelope};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{execute::BlockBuilder, RecoveredTx};
use reth_payload_builder::BlobSidecars;
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::Recovered;
use reth_revm::cancelled::CancelOnDrop;
use tracing::{trace, warn};

use super::{preparation::PayloadContext, size_budget::BlockSizeBudget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StageOutcome {
    Completed,
    Cancelled,
}

pub(super) struct PayloadBuildState {
    pub(super) cumulative_gas_used: u64,
    pub(super) total_fees: U256,
    pub(super) blob_sidecars: BlobSidecars,
    pub(super) block_blob_count: u64,
    pub(super) size_budget: BlockSizeBudget,
    pub(super) block_gas_limit: u64,
    pub(super) base_fee: u64,
    pub(super) reserved_end_gas: u64,
    pub(super) max_blob_count: u64,
    pub(super) is_osaka: bool,
}

impl PayloadBuildState {
    pub(super) fn new(
        context: &PayloadContext<'_>,
        builder_config: &EthereumBuilderConfig,
        block_gas_limit: u64,
        base_fee: u64,
        end_system_txs: &[Recovered<OutbeTxEnvelope>],
    ) -> Result<Self, PayloadBuilderError> {
        let chain_spec = context.chain_spec;
        let inner = context.attributes.inner();
        let blob_params = chain_spec.blob_params_at_timestamp(inner.timestamp);
        let protocol_max_blob_count = blob_params
            .as_ref()
            .map(|params| params.max_blob_count)
            .unwrap_or_default();
        let max_blob_count = builder_config
            .max_blobs_per_block
            .map(|user_limit| std::cmp::min(user_limit, protocol_max_blob_count).max(1))
            .unwrap_or(protocol_max_blob_count);
        let is_osaka = chain_spec.is_osaka_active_at_timestamp(inner.timestamp);
        let withdrawals_rlp_length = inner
            .withdrawals
            .as_ref()
            .map(|withdrawals| withdrawals.length())
            .unwrap_or(0);

        let reserved_end_gas = end_system_txs
            .iter()
            .try_fold(0u64, |total, tx| total.checked_add(tx.tx().gas_limit()))
            .ok_or_else(|| {
                PayloadBuilderError::other(std::io::Error::other(
                    "end system transaction gas overflow",
                ))
            })?;
        let reserved_end_rlp_length = end_system_txs
            .iter()
            .try_fold(0usize, |total, tx| total.checked_add(tx.inner().length()))
            .ok_or_else(|| {
                PayloadBuilderError::other(std::io::Error::other(
                    "end system transaction size overflow",
                ))
            })?;
        let size_budget =
            BlockSizeBudget::new(reserved_end_rlp_length, withdrawals_rlp_length, is_osaka);
        Ok(Self {
            cumulative_gas_used: 0,
            total_fees: U256::ZERO,
            blob_sidecars: BlobSidecars::Empty,
            block_blob_count: 0,
            size_budget,
            block_gas_limit,
            base_fee,
            reserved_end_gas,
            max_blob_count,
            is_osaka,
        })
    }

    pub(super) fn record_user(
        &mut self,
        tx_rlp_len: usize,
        miner_fee: Option<u128>,
        gas_used: u64,
    ) {
        self.size_budget.record(tx_rlp_len);
        let miner_fee = miner_fee.unwrap_or_default();
        self.total_fees += U256::from(miner_fee) * U256::from(gas_used);
        self.cumulative_gas_used += gas_used;
    }

    pub(super) fn execute_begin(
        &mut self,
        builder: &mut impl BlockBuilder<Primitives = OutbePrimitives>,
        transactions: Vec<Recovered<OutbeTxEnvelope>>,
        cancel: &CancelOnDrop,
    ) -> Result<StageOutcome, PayloadBuilderError> {
        for tx in transactions {
            if cancel.is_cancelled() {
                return Ok(StageOutcome::Cancelled);
            }
            let tx_rlp_len = tx.inner().length();
            let gas_used = builder
                .execute_transaction(tx)
                .map_err(|err| {
                    warn!(target: "payload_builder", %err, "failed to execute begin system transaction");
                    PayloadBuilderError::Internal(err.into())
                })?
                .tx_gas_used();
            self.size_budget.record(tx_rlp_len);
            self.cumulative_gas_used = self.cumulative_gas_used.saturating_add(gas_used);
            trace!(
                target: "payload_builder",
                gas_used,
                "included begin system transaction"
            );
        }

        Ok(StageOutcome::Completed)
    }

    pub(super) fn execute_end(
        &mut self,
        builder: &mut impl BlockBuilder<Primitives = OutbePrimitives>,
        transactions: Vec<Recovered<OutbeTxEnvelope>>,
        cancel: &CancelOnDrop,
    ) -> Result<StageOutcome, PayloadBuilderError> {
        for tx in transactions {
            if cancel.is_cancelled() {
                return Ok(StageOutcome::Cancelled);
            }
            let gas_used = builder
                .execute_transaction(tx)
                .map_err(|err| {
                    warn!(target: "payload_builder", %err, "failed to execute end system transaction");
                    PayloadBuilderError::Internal(err.into())
                })?
                .tx_gas_used();
            self.cumulative_gas_used = self.cumulative_gas_used.saturating_add(gas_used);
            trace!(
                target: "payload_builder",
                gas_used,
                "included end system transaction"
            );
        }

        Ok(StageOutcome::Completed)
    }
}
