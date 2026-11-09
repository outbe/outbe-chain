use super::{block_num_reader, TestProvider};
use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{BlockHash, BlockNumber, B256};
use outbe_primitives::header::OutbeHeader;
use reth_chainspec::ChainInfo;
use reth_primitives_traits::SealedHeader;
use reth_provider::{
    BlockHashReader, BlockIdReader, BlockNumReader, HeaderProvider, ProviderResult,
    StateProviderBox, StateProviderFactory,
};
use std::ops::RangeBounds;
use std::sync::{Arc, RwLock};

pub(super) struct FinalizedMockProvider {
    inner: TestProvider,
    finalized: Arc<RwLock<BlockNumHash>>,
    historical_states: Arc<RwLock<std::collections::BTreeMap<B256, TestProvider>>>,
}

impl FinalizedMockProvider {
    pub(super) fn new(inner: TestProvider, finalized: BlockNumHash) -> Self {
        Self {
            inner,
            finalized: Arc::new(RwLock::new(finalized)),
            historical_states: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
        }
    }

    pub(super) fn set_finalized(&self, finalized: BlockNumHash) {
        *self.finalized.write().unwrap() = finalized;
    }

    pub(super) fn override_state(&self, block_hash: B256, state: TestProvider) {
        self.historical_states
            .write()
            .unwrap()
            .insert(block_hash, state);
    }
}

impl BlockHashReader for FinalizedMockProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

block_num_reader::delegate_block_num_reader!(FinalizedMockProvider, inner);

impl BlockIdReader for FinalizedMockProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(*self.finalized.read().unwrap()))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(*self.finalized.read().unwrap()))
    }
}

impl HeaderProvider for FinalizedMockProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: BlockHash) -> ProviderResult<Option<Self::Header>> {
        self.inner.header(block_hash)
    }

    fn header_by_number(&self, number: u64) -> ProviderResult<Option<Self::Header>> {
        self.inner.header_by_number(number)
    }

    fn headers_range(
        &self,
        range: impl RangeBounds<BlockNumber>,
    ) -> ProviderResult<Vec<Self::Header>> {
        self.inner.headers_range(range)
    }

    fn sealed_header(
        &self,
        number: BlockNumber,
    ) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        self.inner.sealed_header(number)
    }

    fn sealed_headers_while(
        &self,
        range: impl RangeBounds<BlockNumber>,
        predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        self.inner.sealed_headers_while(range, predicate)
    }
}

impl StateProviderFactory for FinalizedMockProvider {
    type Primitives = <TestProvider as StateProviderFactory>::Primitives;

    fn latest(&self) -> ProviderResult<StateProviderBox> {
        self.inner.latest()
    }

    fn state_with_block_appended(
        &self,
        parent_hash: BlockHash,
        block: reth_chain_state::ExecutedBlock<Self::Primitives>,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.state_with_block_appended(parent_hash, block)
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        self.inner.state_by_block_number_or_tag(number_or_tag)
    }

    fn history_by_block_number(&self, block: BlockNumber) -> ProviderResult<StateProviderBox> {
        self.inner.history_by_block_number(block)
    }

    fn history_by_block_hash(&self, block: BlockHash) -> ProviderResult<StateProviderBox> {
        self.inner.history_by_block_hash(block)
    }

    fn state_by_block_hash(&self, block: BlockHash) -> ProviderResult<StateProviderBox> {
        if let Some(state) = self.historical_states.read().unwrap().get(&block).cloned() {
            return Ok(Box::new(state));
        }
        self.inner.state_by_block_hash(block)
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        self.inner.pending()
    }

    fn pending_state_by_hash(&self, block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        self.inner.pending_state_by_hash(block_hash)
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        self.inner.maybe_pending()
    }
}
