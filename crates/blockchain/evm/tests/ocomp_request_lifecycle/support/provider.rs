use super::super::*;

pub(in crate::lifecycle) type InnerTestProvider =
    MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;
#[derive(Clone, Debug)]
pub(in crate::lifecycle) struct TestProvider {
    pub(in crate::lifecycle) inner: InnerTestProvider,
    pub(in crate::lifecycle) base_state: Arc<HashedAccountState>,
}

impl TestProvider {
    pub(in crate::lifecycle) fn state_root_for(&self, post_state: HashedPostState) -> B256 {
        state_root_with_overlay(self.base_state.as_ref(), post_state)
    }
}

impl ChainSpecProvider for TestProvider {
    type ChainSpec = ChainSpec<OutbeHeader>;

    fn chain_spec(&self) -> Arc<Self::ChainSpec> {
        self.inner.chain_spec()
    }
}

impl BlockHashReader for TestProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl BlockNumReader for TestProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        self.inner.last_block_number()
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        self.inner.block_number(hash)
    }
}

impl BlockIdReader for TestProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
}

impl AccountReader for TestProvider {
    fn basic_account(&self, address: &Address) -> ProviderResult<Option<Account>> {
        self.inner.basic_account(address)
    }
}

impl BytecodeReader for TestProvider {
    fn bytecode_by_hash(&self, code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        self.inner.bytecode_by_hash(code_hash)
    }
}

impl HashedPostStateProvider for TestProvider {
    fn hashed_post_state(
        &self,
        state: &revm::database::BundleState,
    ) -> ProviderResult<HashedPostState> {
        Ok(HashedPostState::from_bundle_state::<KeccakKeyHasher>(
            state.state(),
        ))
    }
}

impl StateRootProvider for TestProvider {
    fn state_root(&self, post_state: HashedPostState) -> ProviderResult<B256> {
        Ok(self.state_root_for(post_state))
    }

    fn state_root_from_nodes(&self, input: TrieInput) -> ProviderResult<B256> {
        Ok(self.state_root_for(input.state))
    }

    fn state_root_with_updates(
        &self,
        post_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root_for(post_state), TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((self.state_root_for(input.state), TrieUpdates::default()))
    }
}

impl StorageRootProvider for TestProvider {
    fn storage_root(&self, address: Address, post_state: HashedStorage) -> ProviderResult<B256> {
        let hashed_address = keccak256(address);
        let mut storage = self
            .base_state
            .get(&hashed_address)
            .map(|(_, storage)| storage.clone())
            .unwrap_or_default();
        apply_storage_overlay(&mut storage, post_state);
        Ok(storage_root_prehashed(storage))
    }

    fn storage_proof(
        &self,
        address: Address,
        slot: B256,
        post_state: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        self.inner.storage_proof(address, slot, post_state)
    }

    fn storage_multiproof(
        &self,
        address: Address,
        slots: &[B256],
        post_state: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        self.inner.storage_multiproof(address, slots, post_state)
    }
}

impl StateProofProvider for TestProvider {
    fn proof(
        &self,
        input: TrieInput,
        address: Address,
        slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        self.inner.proof(input, address, slots)
    }

    fn multiproof(
        &self,
        input: TrieInput,
        targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        self.inner.multiproof(input, targets)
    }

    fn multiproof_v2(
        &self,
        input: TrieInput,
        targets: reth_trie::MultiProofTargetsV2,
    ) -> ProviderResult<reth_trie::DecodedMultiProofV2> {
        self.inner.multiproof_v2(input, targets)
    }

    fn witness(
        &self,
        input: TrieInput,
        target: HashedPostState,
        mode: ExecutionWitnessMode,
    ) -> ProviderResult<Vec<Bytes>> {
        self.inner.witness(input, target, mode)
    }
}

impl StateProvider for TestProvider {
    fn storage(&self, account: Address, storage_key: B256) -> ProviderResult<Option<U256>> {
        self.inner.storage(account, storage_key)
    }
}

impl StateProviderFactory for TestProvider {
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        Ok(Box::new(self.clone()))
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

    fn state_by_block_hash(&self, _block: B256) -> ProviderResult<StateProviderBox> {
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

    fn state_by_block_id(&self, _block_id: BlockId) -> ProviderResult<StateProviderBox> {
        self.latest()
    }
}

pub(in crate::lifecycle) fn mock_provider(
    chain_spec: &Arc<ChainSpec<OutbeHeader>>,
    storage: &HashMap<(Address, U256), U256>,
) -> TestProvider {
    let inner =
        MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec.as_ref().clone());
    let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
    for ((account, slot), value) in storage {
        accounts
            .entry(*account)
            .or_default()
            .push((B256::from(slot.to_be_bytes::<32>()), *value));
    }
    for (account, account_storage) in accounts {
        inner.add_account(
            account,
            ExtendedAccount::new(0, U256::ZERO)
                .with_bytecode(Bytes::from_static(&[0xef]))
                .extend_storage(account_storage),
        );
    }
    for validator_index in 0_u8..4 {
        inner.add_account(
            validator_sender(validator_index),
            ExtendedAccount::new(0, vote_sender_balance()),
        );
    }
    inner.add_account(
        saturated_user_sender(),
        ExtendedAccount::new(0, vote_sender_balance()),
    );
    inner.add_account(
        BURNER_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO)
            .with_bytecode(Bytes::from_static(&[0x5b, 0x60, 0x00, 0x56])),
    );
    TestProvider {
        inner,
        base_state: Arc::new(hashed_marker_state(storage)),
    }
}
