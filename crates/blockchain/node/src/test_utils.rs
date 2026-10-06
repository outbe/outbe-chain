//! Stored Ethereum opening and historical-block providers for test fixtures.

use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{Address, Bytes, B256, U256};
use reth_chainspec::ChainInfo;
use reth_primitives_traits::{Account, Bytecode};
use reth_storage_api::{
    errors::provider::ProviderResult, AccountReader, BlockHashReader, BlockIdReader,
    BlockNumReader, BytecodeReader, HashedPostStateProvider, StateProofProvider, StateProvider,
    StateProviderBox, StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_trie::{
    updates::TrieUpdates, AccountProof, ExecutionWitnessMode, HashedPostState, HashedStorage,
    KeccakKeyHasher, MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

/// Observe raw storage reads without affecting proof or account lookup.
pub trait StorageReadObserver: Clone + Send + Sync + 'static {
    fn record_read(&self);
}
impl StorageReadObserver for () {
    fn record_read(&self) {}
}
impl StorageReadObserver for Arc<AtomicUsize> {
    fn record_read(&self) {
        self.fetch_add(1, Ordering::Relaxed);
    }
}

/// State-only fixtures intentionally expose no block-history lookup.
#[derive(Clone, Copy, Default)]
pub struct NoBlockLookup;
impl BlockHashReader for NoBlockLookup {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(&self, _start: u64, _end: u64) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

/// A fixture's sole canonical block identity.
#[derive(Clone, Copy)]
pub struct ExactBlockLookup {
    pub identity: BlockNumHash,
}
impl BlockHashReader for ExactBlockLookup {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok((number == self.identity.number).then_some(self.identity.hash))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| (number == self.identity.number).then_some(self.identity.hash))
            .collect())
    }
}

/// Stored accounts, values and proof paths with explicit read/lookup policies.
#[derive(Clone)]
pub struct OpeningStateFixture<R = (), B = NoBlockLookup> {
    pub state_root: B256,
    pub accounts: BTreeMap<Address, Account>,
    pub storage: BTreeMap<(Address, B256), U256>,
    pub proofs: BTreeMap<Address, AccountProof>,
    pub storage_reads: R,
    pub block_lookup: B,
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> AccountReader
    for OpeningStateFixture<R, B>
{
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        Ok(self.accounts.get(address).copied())
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> BlockHashReader
    for OpeningStateFixture<R, B>
{
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.block_lookup.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.block_lookup.canonical_hashes_range(start, end)
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> BytecodeReader
    for OpeningStateFixture<R, B>
{
    fn bytecode_by_hash(&self, _code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        Ok(None)
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> StateRootProvider
    for OpeningStateFixture<R, B>
{
    fn state_root(&self, _hashed_state: HashedPostState) -> ProviderResult<B256> {
        Ok(self.state_root)
    }

    fn state_root_from_nodes(&self, _input: TrieInput) -> ProviderResult<B256> {
        Ok(self.state_root)
    }

    fn state_root_with_updates(
        &self,
        _hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root, TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root, TrieUpdates::default()))
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> StorageRootProvider
    for OpeningStateFixture<R, B>
{
    fn storage_root(
        &self,
        address: Address,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<B256> {
        Ok(self
            .proofs
            .get(&address)
            .map_or(B256::ZERO, |proof| proof.storage_root))
    }

    fn storage_proof(
        &self,
        address: Address,
        slot: B256,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Ok(self
            .proofs
            .get(&address)
            .and_then(|proof| proof.storage_proofs.iter().find(|proof| proof.key == slot))
            .cloned()
            .unwrap_or_else(|| StorageProof::new(slot)))
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Ok(StorageMultiProof::empty())
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> StateProofProvider
    for OpeningStateFixture<R, B>
{
    fn proof(
        &self,
        _input: TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        let mut proof = self
            .proofs
            .get(&address)
            .cloned()
            .unwrap_or_else(|| AccountProof::new(address));
        proof.storage_proofs = slots
            .iter()
            .map(|slot| {
                proof
                    .storage_proofs
                    .iter()
                    .find(|storage| storage.key == *slot)
                    .cloned()
                    .unwrap_or_else(|| StorageProof::new(*slot))
            })
            .collect();
        Ok(proof)
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        _targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        Ok(MultiProof::default())
    }

    fn multiproof_v2(
        &self,
        _input: TrieInput,
        _targets: reth_trie::MultiProofTargetsV2,
    ) -> ProviderResult<reth_trie::DecodedMultiProofV2> {
        Ok(reth_trie::DecodedMultiProofV2::default())
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
        _mode: ExecutionWitnessMode,
    ) -> ProviderResult<Vec<Bytes>> {
        Ok(Vec::new())
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static>
    HashedPostStateProvider for OpeningStateFixture<R, B>
{
    fn hashed_post_state(
        &self,
        bundle_state: &revm::database::BundleState,
    ) -> ProviderResult<HashedPostState> {
        Ok(HashedPostState::from_bundle_state::<KeccakKeyHasher>(
            bundle_state.state(),
        ))
    }
}

impl<R: StorageReadObserver, B: BlockHashReader + Clone + Send + Sync + 'static> StateProvider
    for OpeningStateFixture<R, B>
{
    fn storage(&self, account: Address, storage_key: B256) -> ProviderResult<Option<U256>> {
        self.storage_reads.record_read();
        Ok(self.storage.get(&(account, storage_key)).copied())
    }
}

/// A stored state pinned to one historical block for opening builders.
#[derive(Clone)]
pub struct OpeningProviderFixture<R = ()> {
    pub state: OpeningStateFixture<R>,
    pub block: ExactBlockLookup,
    pub exact_hash_message: &'static str,
}

impl<R: StorageReadObserver> BlockHashReader for OpeningProviderFixture<R> {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.block.block_hash(number)
    }
    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.block.canonical_hashes_range(start, end)
    }
}

impl<R: StorageReadObserver> BlockNumReader for OpeningProviderFixture<R> {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.block.identity.hash,
            best_number: self.block.identity.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block.identity.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block.identity.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok((hash == self.block.identity.hash).then_some(self.block.identity.number))
    }
}

impl<R: StorageReadObserver> BlockIdReader for OpeningProviderFixture<R> {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(
            self.block.identity.number,
            self.block.identity.hash,
        )))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(
            self.block.identity.number,
            self.block.identity.hash,
        )))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(
            self.block.identity.number,
            self.block.identity.hash,
        )))
    }
}

impl<R: StorageReadObserver> StateProviderFactory for OpeningProviderFixture<R> {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(self.state.clone()))
    }

    fn state_by_block_number_or_tag(
        &self,
        _number_or_tag: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn history_by_block_number(&self, _block: u64) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn history_by_block_hash(&self, _block: B256) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn state_by_block_hash(&self, block: B256) -> ProviderResult<StateProviderBox> {
        assert_eq!(
            block, self.block.identity.hash,
            "{}",
            self.exact_hash_message
        );
        self.latest()
    }

    fn pending(&self) -> ProviderResult<StateProviderBox> {
        self.latest()
    }

    fn pending_state_by_hash(&self, _block_hash: B256) -> ProviderResult<Option<StateProviderBox>> {
        self.latest().map(Some)
    }

    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        self.latest().map(Some)
    }
}
