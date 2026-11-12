use super::*;
pub(crate) struct SeededTeeState {
    pub(crate) valid_until: u64,
    pub(crate) has_validator_binding: bool,
    pub(crate) validator_binding_mismatch: bool,
    pub(crate) validator_jailed: bool,
    pub(crate) offer_key: alloy_primitives::B256,
}

#[derive(Clone)]
pub(crate) struct FinalizedMockProvider {
    pub(crate) inner: reth_provider::test_utils::MockEthProvider<outbe_primitives::OutbePrimitives>,
    finalized: Option<alloy_eips::BlockNumHash>,
    pub(crate) historical_states: std::collections::HashMap<
        alloy_primitives::B256,
        reth_provider::test_utils::MockEthProvider<outbe_primitives::OutbePrimitives>,
    >,
    pub(crate) fail_state_read: bool,
}

impl reth_provider::BlockHashReader for FinalizedMockProvider {
    fn block_hash(
        &self,
        number: u64,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(
        &self,
        start: u64,
        end: u64,
    ) -> reth_provider::ProviderResult<Vec<alloy_primitives::B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl reth_provider::BlockNumReader for FinalizedMockProvider {
    fn chain_info(&self) -> reth_provider::ProviderResult<reth_chainspec::ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.inner.last_block_number()
    }

    fn block_number(
        &self,
        hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<u64>> {
        self.inner.block_number(hash)
    }
}

impl reth_provider::BlockIdReader for FinalizedMockProvider {
    fn pending_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(self.finalized)
    }

    fn finalized_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(self.finalized)
    }
}

impl reth_provider::HeaderProvider for FinalizedMockProvider {
    type Header = OutbeHeader;

    fn header(
        &self,
        block_hash: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.inner.header(block_hash)
    }

    fn header_by_number(&self, number: u64) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.inner.header_by_number(number)
    }

    fn headers_range(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
    ) -> reth_provider::ProviderResult<Vec<Self::Header>> {
        self.inner.headers_range(range)
    }

    fn sealed_header(
        &self,
        number: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<Option<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.inner.sealed_header(number)
    }

    fn sealed_headers_while(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
        predicate: impl FnMut(&reth_primitives_traits::SealedHeader<Self::Header>) -> bool,
    ) -> reth_provider::ProviderResult<Vec<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.inner.sealed_headers_while(range, predicate)
    }
}

impl reth_provider::StateProviderFactory for FinalizedMockProvider {
    type Primitives = <reth_provider::test_utils::MockEthProvider<
        outbe_primitives::OutbePrimitives,
    > as reth_provider::StateProviderFactory>::Primitives;

    fn latest(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.latest()
    }

    fn state_with_block_appended(
        &self,
        parent_hash: alloy_primitives::BlockHash,
        block: reth_chain_state::ExecutedBlock<Self::Primitives>,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.state_with_block_appended(parent_hash, block)
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.state_by_block_number_or_tag(number_or_tag)
    }

    fn history_by_block_number(
        &self,
        block: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.history_by_block_number(block)
    }

    fn history_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.history_by_block_hash(block)
    }

    fn state_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        if self.fail_state_read {
            return Err(reth_provider::ProviderError::BestBlockNotFound);
        }
        if let Some(historical) = self.historical_states.get(&block) {
            return historical.state_by_block_hash(block);
        }
        self.inner.state_by_block_hash(block)
    }

    fn pending(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.pending()
    }

    fn pending_state_by_hash(
        &self,
        block_hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.inner.pending_state_by_hash(block_hash)
    }

    fn maybe_pending(
        &self,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.inner.maybe_pending()
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn build_tee_admission_mock_provider(
    blocks: &[(u64, u64)],
    state_cfg: SeededTeeState,
) -> (
    FinalizedMockProvider,
    outbe_execution::tee_remote_session::RegistryChainIdentity,
    outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    Vec<(u64, alloy_primitives::B256)>,
) {
    use alloy_consensus::Header;
    use alloy_primitives::{Address, B256, U256};
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::tee_attestation_v1::NodeIdV1;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use std::collections::HashMap;

    let chain_id = 676_u64;
    let genesis_hash = B256::repeat_byte(0x11);
    let validator = Address::with_last_byte(0x31);
    let owner = Address::repeat_byte(0xA0);
    let reth_p2p_public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[0x31; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let node_hash = NodeIdV1 { reth_p2p_public }.node_id_hash().unwrap();
    let enclave_id = B256::repeat_byte(0x32);
    let binding_id = B256::repeat_byte(0x33);

    let mut storage_provider = HashMapStorageProvider::new(chain_id);
    storage_provider.set_block_number(blocks.last().map_or(1, |(n, _)| *n));
    StorageHandle::enter(&mut storage_provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(owner).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let key = bls12381::PrivateKey::from_seed(31);
        let encoded = key.public_key().encode();
        let mut pubkey = [0u8; 48];
        pubkey.copy_from_slice(&encoded[..48]);
        let minimum = U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(owner, validator, &pubkey).unwrap();
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        if state_cfg.validator_jailed {
            vs.jail_validator_for_tee_expiry(validator).unwrap();
        }

        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .v1_node_enclave_id
            .write(&node_hash, enclave_id)
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, binding_id)
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::repeat_byte(0x34))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, state_cfg.valid_until)
            .unwrap();
        if !state_cfg.offer_key.is_zero() {
            registry
                .tribute_offer_public_key
                .write(state_cfg.offer_key)
                .unwrap();
        }
        if state_cfg.has_validator_binding {
            let bound_node = if state_cfg.validator_binding_mismatch {
                let other_node = B256::repeat_byte(0x99);
                registry
                    .v1_node_enclave_id
                    .write(&other_node, enclave_id)
                    .unwrap();
                registry
                    .v1_node_binding_id
                    .write(&other_node, binding_id)
                    .unwrap();
                registry
                    .v1_node_intent_hash
                    .write(&other_node, B256::repeat_byte(0x34))
                    .unwrap();
                registry
                    .v1_node_valid_until
                    .write(&other_node, state_cfg.valid_until)
                    .unwrap();
                other_node
            } else {
                node_hash
            };
            registry
                .validator_v1_node_hash
                .write(&validator, bound_node)
                .unwrap();
        }
    });

    let provider = MockEthProvider::<outbe_primitives::OutbePrimitives>::new();
    let mut by_address: HashMap<Address, Vec<(B256, U256)>> = HashMap::new();
    for ((address, slot), value) in storage_provider.storage {
        by_address
            .entry(address)
            .or_default()
            .push((B256::from(slot.to_be_bytes()), value));
    }
    for (address, slots) in by_address {
        provider.add_account(
            address,
            ExtendedAccount::new(0, U256::ZERO).extend_storage(slots),
        );
    }

    let mut hashes = Vec::with_capacity(blocks.len());
    for &(number, timestamp) in blocks {
        let header = OutbeHeader::new(Header {
            number,
            timestamp,
            ..Default::default()
        });
        let hash = alloy_consensus::Sealable::hash_slow(&header);
        provider.add_header(hash, header);
        hashes.push((number, hash));
    }
    if !blocks.iter().any(|(number, _)| *number == 0) {
        let header = OutbeHeader::new(Header {
            number: 0,
            timestamp: 1_700_000_000,
            ..Default::default()
        });
        let hash = alloy_consensus::Sealable::hash_slow(&header);
        provider.add_header(hash, header);
    }

    let finalized = hashes
        .last()
        .map(|&(number, hash)| alloy_eips::BlockNumHash::new(number, hash));

    (
        FinalizedMockProvider {
            inner: provider,
            finalized,
            historical_states: HashMap::new(),
            fail_state_read: false,
        },
        outbe_execution::tee_remote_session::RegistryChainIdentity {
            chain_id,
            genesis_hash,
        },
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
            reth_p2p_public,
            expected_enclave_id: Some(enclave_id),
            validator: Some(validator),
        },
        hashes,
    )
}
