//! Reads native runtime state through the canonical Reth provider.
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::storage::readonly::StorageReader;
use reth_storage_api::StateProvider;

/// Bridges Reth's state provider into Outbe's read-only precompile storage.
pub(crate) struct RethStateReader<'a, P> {
    state: &'a P,
}

impl<'a, P> RethStateReader<'a, P> {
    pub(crate) fn new(state: &'a P) -> Self {
        Self { state }
    }
}

impl<P> StorageReader for RethStateReader<'_, P>
where
    P: StateProvider,
{
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|e| crate::errors::TxPoolStateReadError::StateReadFailed(e.to_string()).into())
    }
}
