use crate::factory::OutbeEvmFactory;
use outbe_primitives::{OutbeBlock, OutbeHeader};
use reth_ethereum::{chainspec::ChainSpec, evm::EthBlockAssembler};
use reth_evm::execute::{BlockAssembler, BlockAssemblerInput, BlockExecutionError};
use reth_primitives_traits::SealedHeader;
use std::sync::Arc;

use super::OutbeEvmConfig;

#[derive(Debug, Clone)]
pub struct OutbeBlockAssembler {
    inner: EthBlockAssembler<ChainSpec<OutbeHeader>>,
}

impl OutbeBlockAssembler {
    pub fn new(chain_spec: Arc<ChainSpec<OutbeHeader>>) -> Self {
        Self {
            inner: EthBlockAssembler::new(chain_spec),
        }
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
            output,
            bundle_state,
            state_provider,
            state_root,
            block_access_list_hash,
            ..
        } = input;

        let parent = SealedHeader::new_unhashed(parent.clone().into_header().into_inner());

        let block = self.inner.assemble_block(
            BlockAssemblerInput::<
                alloy_evm::eth::EthBlockExecutorFactory<
                    reth_ethereum::evm::RethReceiptBuilder,
                    Arc<ChainSpec<OutbeHeader>>,
                    OutbeEvmFactory,
                >,
            >::new(
                evm_env,
                execution_ctx.inner,
                &parent,
                transactions,
                output,
                bundle_state,
                state_provider,
                state_root,
                block_access_list_hash,
            ),
            None,
            None,
            None,
        )?;

        // `inner.extra_data` already encodes `timestamp_millis_part`
        // (under tag 0x05) - see `OutbeBlockBuilder::finish` in
        // `crates/blockchain/evm/src/builder.rs`. The wrapper carries
        // no extra RLP fields, so the resulting block hash is exactly
        // `keccak256(rlp(standard_ethereum_header))`.
        Ok(block.map_header(OutbeHeader::new))
    }
}
