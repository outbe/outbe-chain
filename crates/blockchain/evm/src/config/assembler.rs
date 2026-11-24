use alloy_consensus::{
    proofs::{self, calculate_receipt_root},
    Block, BlockBody, BlockHeader as _, Header, TxReceipt, EMPTY_OMMER_ROOT_HASH,
};
use alloy_eips::{eip4895::Withdrawals, merge::BEACON_NONCE};
use alloy_primitives::{Bloom, B256};
use outbe_primitives::{OutbeBlock, OutbeHeader};
use reth_ethereum::chainspec::{ChainSpec, EthChainSpec, EthereumHardforks};
use reth_ethereum::provider::BlockExecutionResult;
use reth_evm::{BlockAssembler, BlockAssemblerInput, BlockExecutionError, EvmEnv as _};
use std::sync::Arc;

use super::OutbeEvmConfig;

#[derive(Debug, Clone)]
pub struct OutbeBlockAssembler {
    chain_spec: Arc<ChainSpec<OutbeHeader>>,
}

impl OutbeBlockAssembler {
    pub const fn new(chain_spec: Arc<ChainSpec<OutbeHeader>>) -> Self {
        Self { chain_spec }
    }
}

impl BlockAssembler<OutbeEvmConfig> for OutbeBlockAssembler {
    type Block = OutbeBlock;

    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, OutbeEvmConfig, OutbeHeader>,
    ) -> Result<Self::Block, BlockExecutionError> {
        let BlockAssemblerInput {
            evm_env,
            execution_ctx,
            parent,
            transactions,
            output:
                BlockExecutionResult {
                    receipts,
                    requests,
                    gas_used,
                    blob_gas_used,
                },
            state_root,
            block_access_list_hash,
            ..
        } = input;
        let ctx = execution_ctx.inner;
        let block = evm_env.block_env();

        let timestamp = block.timestamp.to::<u64>();
        let transactions_root = proofs::calculate_transaction_root(&transactions);
        let receipts_with_bloom = receipts
            .iter()
            .map(TxReceipt::with_bloom_ref)
            .collect::<Vec<_>>();
        let receipts_root = calculate_receipt_root(&receipts_with_bloom);
        let logs_bloom = receipts_with_bloom
            .iter()
            .fold(Bloom::ZERO, |acc, r| acc | r.bloom_ref());

        let withdrawals = self
            .chain_spec
            .is_shanghai_active_at_timestamp(timestamp)
            .then(|| Withdrawals::new(ctx.withdrawals.map(|w| w.into_owned()).unwrap_or_default()));
        let withdrawals_root = withdrawals
            .as_deref()
            .map(|w| proofs::calculate_withdrawals_root(w));
        let requests_hash = self
            .chain_spec
            .is_prague_active_at_timestamp(timestamp)
            .then(|| requests.requests_hash());
        let block_number = block.number.to::<u64>();
        let base_fee_per_gas = self
            .chain_spec
            .is_london_active_at_block(block_number)
            .then(|| block.basefee.to::<u64>());

        let mut excess_blob_gas = None;
        let mut block_blob_gas_used = None;
        if self.chain_spec.is_cancun_active_at_timestamp(timestamp) {
            block_blob_gas_used = Some(*blob_gas_used);
            excess_blob_gas = if self
                .chain_spec
                .is_cancun_active_at_timestamp(parent.header().inner.timestamp)
            {
                parent.header().inner.maybe_next_block_excess_blob_gas(
                    self.chain_spec.blob_params_at_timestamp(timestamp),
                )
            } else {
                Some(
                    alloy_eips::eip7840::BlobParams::cancun()
                        .next_block_excess_blob_gas_osaka(0, 0, 0),
                )
            };
        }

        let header = Header {
            parent_hash: ctx.parent_hash,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: block.beneficiary,
            state_root,
            transactions_root,
            receipts_root,
            withdrawals_root,
            logs_bloom,
            timestamp,
            mix_hash: B256::from(block.prevrandao),
            nonce: BEACON_NONCE.into(),
            base_fee_per_gas,
            number: block_number,
            gas_limit: block.gas_limit.to(),
            difficulty: block.difficulty,
            gas_used: *gas_used,
            extra_data: ctx.extra_data,
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            blob_gas_used: block_blob_gas_used,
            excess_blob_gas,
            requests_hash,
            block_access_list_hash,
            slot_number: ctx.slot_number,
        };

        // `inner.extra_data` already encodes `timestamp_millis_part`
        // (under tag 0x05) - see `OutbeBlockBuilder::finish_with_state_root` in
        // `crates/blockchain/evm/src/builder.rs`. The wrapper carries
        // no extra RLP fields, so the resulting block hash is exactly
        // `keccak256(rlp(standard_ethereum_header))`.
        Ok(Block {
            header: OutbeHeader::new(header),
            body: BlockBody {
                transactions,
                ommers: Default::default(),
                withdrawals,
            },
        })
    }
}
