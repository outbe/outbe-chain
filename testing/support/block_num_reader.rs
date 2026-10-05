//! Shared block-number reader delegation for integration-test providers.

macro_rules! delegate_block_num_reader {
    ($provider:ty, $inner:ident) => {
        impl BlockNumReader for $provider {
            fn chain_info(&self) -> ProviderResult<ChainInfo> {
                self.$inner.chain_info()
            }

            fn best_block_number(&self) -> ProviderResult<u64> {
                self.$inner.best_block_number()
            }

            fn last_block_number(&self) -> ProviderResult<u64> {
                self.$inner.last_block_number()
            }

            fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
                self.$inner.block_number(hash)
            }
        }
    };
}

pub(crate) use delegate_block_num_reader;
