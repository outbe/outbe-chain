use std::collections::BTreeMap;

use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_consensus::block::ConsensusBlock;
use outbe_node::ocomp::finality::{PublicAccountProofV1, PublicBlockViewV1};
use outbe_ocomp_protocol::{
    intent::{FinalizedIntentProofV1, JobIntentV1},
    opening::OpeningSubjectsV1,
};
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

/// Authenticated inputs generated for one exact process-test intent.
pub struct FinalizedIntentProofFixture {
    pub intent: JobIntentV1,
    pub intent_id: B256,
    pub job_id: B256,
    pub proof: FinalizedIntentProofV1,
    pub state_root: B256,
    pub header_hash: B256,
    pub block: ConsensusBlock,
    pub canonical_history: CanonicalHistoryFixture,
    pub public_exact_block: PublicExactBlockFixtureV1,
}

#[derive(Clone)]
pub struct PublicExactBlockFixtureV1 {
    pub finalization_bytes: Vec<u8>,
    pub block_bytes: Vec<u8>,
    pub block_view: PublicBlockViewV1,
    pub intent_id: B256,
    pub canonical_job_record: Vec<u8>,
    pub account_proofs: BTreeMap<Address, PublicAccountProofV1>,
}

/// Finalized JobIntent plus an exact historical state provider for the
/// production Fidelity/Oracle opening builder.
pub struct FinalizedLysisInputFixture {
    pub finalized: FinalizedIntentProofFixture,
    pub opening_provider: LysisOpeningProvider,
    pub subjects: OpeningSubjectsV1,
}

#[derive(Clone)]
pub struct LysisOpeningProvider {
    pub(crate) state: LysisOpeningState,
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
}

#[derive(Clone)]
pub(crate) struct LysisOpeningState {
    pub(crate) state_root: B256,
    pub(crate) accounts: BTreeMap<Address, Account>,
    pub(crate) storage: BTreeMap<(Address, B256), U256>,
    pub(crate) proofs: BTreeMap<Address, AccountProof>,
}

/// Minimal exact canonical-history provider used by the production containment
/// adapter in process tests.
#[derive(Clone)]
pub struct CanonicalHistoryFixture {
    hashes: BTreeMap<u64, B256>,
    finalized: BlockNumHash,
}

impl CanonicalHistoryFixture {
    #[must_use]
    pub fn new(block_number: u64, block_hash: B256) -> Self {
        Self {
            hashes: BTreeMap::from([(block_number, block_hash)]),
            finalized: BlockNumHash::new(block_number, block_hash),
        }
    }

    #[must_use]
    pub fn with_canonical_block(mut self, block_number: u64, block_hash: B256) -> Self {
        self.hashes.insert(block_number, block_hash);
        if block_number > self.finalized.number {
            self.finalized = BlockNumHash::new(block_number, block_hash);
        }
        self
    }
}

impl BlockHashReader for CanonicalHistoryFixture {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.hashes.get(&number).copied())
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| self.hashes.get(&number).copied())
            .collect())
    }
}

impl BlockNumReader for CanonicalHistoryFixture {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.finalized.hash,
            best_number: self.finalized.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok(self
            .hashes
            .iter()
            .find_map(|(number, candidate)| (*candidate == hash).then_some(*number)))
    }
}

impl BlockIdReader for CanonicalHistoryFixture {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }
}

impl AccountReader for LysisOpeningState {
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        Ok(self.accounts.get(address).copied())
    }
}

impl BlockHashReader for LysisOpeningState {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(&self, _start: u64, _end: u64) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

impl BytecodeReader for LysisOpeningState {
    fn bytecode_by_hash(&self, _code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        Ok(None)
    }
}

impl StateRootProvider for LysisOpeningState {
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

impl StorageRootProvider for LysisOpeningState {
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

impl StateProofProvider for LysisOpeningState {
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

impl HashedPostStateProvider for LysisOpeningState {
    fn hashed_post_state(
        &self,
        bundle_state: &revm::database::BundleState,
    ) -> ProviderResult<HashedPostState> {
        Ok(HashedPostState::from_bundle_state::<KeccakKeyHasher>(
            bundle_state.state(),
        ))
    }
}

impl StateProvider for LysisOpeningState {
    fn storage(&self, account: Address, storage_key: B256) -> ProviderResult<Option<U256>> {
        Ok(self.storage.get(&(account, storage_key)).copied())
    }
}

impl BlockHashReader for LysisOpeningProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok((number == self.block_number).then_some(self.block_hash))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| (number == self.block_number).then_some(self.block_hash))
            .collect())
    }
}

impl BlockNumReader for LysisOpeningProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.block_hash,
            best_number: self.block_number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block_number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.block_number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok((hash == self.block_hash).then_some(self.block_number))
    }
}

impl BlockIdReader for LysisOpeningProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(BlockNumHash::new(self.block_number, self.block_hash)))
    }
}

impl StateProviderFactory for LysisOpeningProvider {
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
            block, self.block_hash,
            "opening builder must request the exact finalized block hash"
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
