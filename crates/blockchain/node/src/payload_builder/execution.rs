//! Per-attempt budgets and system-transaction execution.

use alloy_primitives::U256;
use alloy_rlp::Encodable as _;
use outbe_primitives::{OutbePrimitives, OutbeTxEnvelope};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{execute::BlockBuilder, Evm};
use reth_payload_builder::BlobSidecars;
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::Recovered;
use reth_tasks::CancelOnDrop;
use tracing::{trace, warn};

use super::{preparation::PayloadContext, size_budget::BlockSizeBudget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StageOutcome {
    Completed,
    Cancelled,
}

/// Read committed counters without restricting builder wrappers to a concrete type.
pub(super) trait PayloadGasAccounting {
    fn gas_accounting(&self) -> (u64, u64, u64);
    fn tx_gas_limit_cap(&self) -> u64;
}

impl<E> PayloadGasAccounting for outbe_evm::executor::OutbeBlockExecutor<'_, E>
where
    E: Evm,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
{
    fn gas_accounting(&self) -> (u64, u64, u64) {
        Self::gas_accounting(self)
    }

    fn tx_gas_limit_cap(&self) -> u64 {
        Self::tx_gas_limit_cap(self)
    }
}

pub(super) struct PayloadBuildState {
    pub(super) cumulative_gas_used: u64,
    pub(super) total_fees: U256,
    pub(super) blob_sidecars: BlobSidecars,
    pub(super) block_blob_count: u64,
    pub(super) size_budget: BlockSizeBudget,
    pub(super) pre_end_gas_limit: u64,
    pub(super) regular_gas_used: u64,
    pub(super) state_gas_used: u64,
    pub(super) eip8037: bool,
    pub(super) tx_gas_limit_cap: u64,
    pub(super) base_fee: u64,
    pub(super) max_blob_count: u64,
    pub(super) is_osaka: bool,
}

impl PayloadBuildState {
    pub(super) fn new(
        context: &PayloadContext<'_>,
        builder_config: &EthereumBuilderConfig,
        block_gas_limit: u64,
        base_fee: u64,
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

        let size_budget = BlockSizeBudget::new(withdrawals_rlp_length, is_osaka);
        let gas_budget = outbe_primitives::system_tx::BlockGasBudget::new(block_gas_limit)
            .map_err(PayloadBuilderError::other)?;
        Ok(Self {
            cumulative_gas_used: 0,
            total_fees: U256::ZERO,
            blob_sidecars: BlobSidecars::Empty,
            block_blob_count: 0,
            size_budget,
            pre_end_gas_limit: gas_budget.pre_end_capacity(),
            regular_gas_used: 0,
            state_gas_used: 0,
            eip8037: false,
            tx_gas_limit_cap: u64::MAX,
            base_fee,
            max_blob_count,
            is_osaka,
        })
    }

    pub(super) fn sync_gas<B>(&mut self, builder: &B)
    where
        B: BlockBuilder<Primitives = OutbePrimitives>,
        B::Executor: PayloadGasAccounting,
    {
        (
            self.cumulative_gas_used,
            self.regular_gas_used,
            self.state_gas_used,
        ) = builder.executor().gas_accounting();
        self.eip8037 = builder.evm().cfg_env().enable_amsterdam_eip8037;
        self.tx_gas_limit_cap = builder.executor().tx_gas_limit_cap();
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
    }

    pub(super) fn execute_begin<B>(
        &mut self,
        builder: &mut B,
        transactions: Vec<Recovered<OutbeTxEnvelope>>,
        cancel: &CancelOnDrop,
    ) -> Result<StageOutcome, PayloadBuilderError>
    where
        B: BlockBuilder<Primitives = OutbePrimitives>,
        B::Executor: PayloadGasAccounting,
    {
        self.sync_gas(builder);
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
            self.sync_gas(builder);
            trace!(
                target: "payload_builder",
                gas_used,
                "included begin system transaction"
            );
        }

        Ok(StageOutcome::Completed)
    }
}
