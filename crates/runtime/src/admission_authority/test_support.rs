use crate::launch_tests::admission_provider as provider;
use crate::*;
use alloy_primitives::{B256, U256};
use outbe_primitives::tee_attestation_v1::*;
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::{ResidentOfferMaterial, ResidentTeeKeys},
};

pub(crate) fn context(seed: u8) -> Arc<InProcessTeeContext> {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let node = k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x11),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: NodeIdV1 {
            reth_p2p_public: node
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: vec![],
    };
    let identity = outbe_tee::NodeHostIdentityV1 {
        network_binding: descriptor.network_binding,
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let context = Arc::new(
        InProcessTeeContext::new(
            keys,
            identity,
            manifest,
            descriptor,
            ProofSourceV1::NoHardware,
        )
        .unwrap(),
    );
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![7; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    context
}

pub(crate) fn provider(
    context: &InProcessTeeContext,
    height: u64,
    timestamp: u64,
    valid_until: u64,
    jailed: bool,
) -> (
    provider::FinalizedMockProvider,
    outbe_execution::tee_remote_session::RegistryChainIdentity,
    outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
) {
    let (provider, chain, mut identity, _) = provider::build_tee_admission_mock_provider(
        &[(height, timestamp)],
        provider::SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: jailed,
            offer_key: B256::from(context.network_encryption_public().unwrap()),
        },
    );
    let enclave = context.manifest().enclave_id().unwrap();
    identity.expected_enclave_id = Some(enclave);
    bind_context(&provider, context);
    (provider, chain, identity)
}

pub(crate) fn bind_context(
    provider: &provider::FinalizedMockProvider,
    context: &InProcessTeeContext,
) {
    let enclave = context.manifest().enclave_id().unwrap();
    let mut storage = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(676);
    outbe_primitives::storage::StorageHandle::enter(&mut storage, |storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .v1_node_enclave_id
            .write(&context.manifest().node_id.node_id_hash().unwrap(), enclave)
            .unwrap();
    });
    let mut accounts = provider.inner.accounts.lock();
    for ((address, slot), value) in storage.storage {
        let account = accounts
            .get(&address)
            .unwrap()
            .clone()
            .extend_storage([(B256::from(slot.to_be_bytes()), value)]);
        accounts.insert(address, account);
    }
    drop(accounts);
}

#[derive(Clone)]
pub(crate) struct SharedProvider(pub(crate) Arc<std::sync::Mutex<provider::FinalizedMockProvider>>);
impl reth_provider::BlockHashReader for SharedProvider {
    fn block_hash(
        &self,
        number: u64,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::B256>> {
        self.0.lock().unwrap().block_hash(number)
    }

    fn canonical_hashes_range(
        &self,
        start: u64,
        end: u64,
    ) -> reth_provider::ProviderResult<Vec<alloy_primitives::B256>> {
        self.0.lock().unwrap().canonical_hashes_range(start, end)
    }
}

impl reth_provider::BlockNumReader for SharedProvider {
    fn chain_info(&self) -> reth_provider::ProviderResult<reth_chainspec::ChainInfo> {
        self.0.lock().unwrap().chain_info()
    }

    fn best_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.0.lock().unwrap().best_block_number()
    }

    fn last_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.0.lock().unwrap().last_block_number()
    }

    fn block_number(
        &self,
        hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<u64>> {
        self.0.lock().unwrap().block_number(hash)
    }
}

impl reth_provider::BlockIdReader for SharedProvider {
    fn pending_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        self.0.lock().unwrap().finalized_block_num_hash()
    }

    fn finalized_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        self.0.lock().unwrap().finalized_block_num_hash()
    }
}

impl reth_provider::HeaderProvider for SharedProvider {
    type Header = OutbeHeader;

    fn header(
        &self,
        block_hash: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.0.lock().unwrap().header(block_hash)
    }

    fn header_by_number(&self, number: u64) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.0.lock().unwrap().header_by_number(number)
    }

    fn headers_range(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
    ) -> reth_provider::ProviderResult<Vec<Self::Header>> {
        self.0.lock().unwrap().headers_range(range)
    }

    fn sealed_header(
        &self,
        number: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<Option<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.0.lock().unwrap().sealed_header(number)
    }

    fn sealed_headers_while(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
        predicate: impl FnMut(&reth_primitives_traits::SealedHeader<Self::Header>) -> bool,
    ) -> reth_provider::ProviderResult<Vec<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.0
            .lock()
            .unwrap()
            .sealed_headers_while(range, predicate)
    }
}

impl reth_provider::StateProviderFactory for SharedProvider {
    type Primitives = <reth_provider::test_utils::MockEthProvider<
        outbe_primitives::OutbePrimitives,
    > as reth_provider::StateProviderFactory>::Primitives;

    fn latest(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0.lock().unwrap().latest()
    }

    fn state_with_block_appended(
        &self,
        parent_hash: alloy_primitives::BlockHash,
        block: reth_chain_state::ExecutedBlock<Self::Primitives>,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0
            .lock()
            .unwrap()
            .state_with_block_appended(parent_hash, block)
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0
            .lock()
            .unwrap()
            .state_by_block_number_or_tag(number_or_tag)
    }

    fn history_by_block_number(
        &self,
        block: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0.lock().unwrap().history_by_block_number(block)
    }

    fn history_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0.lock().unwrap().history_by_block_hash(block)
    }

    fn state_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0.lock().unwrap().state_by_block_hash(block)
    }

    fn pending(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.0.lock().unwrap().pending()
    }

    fn pending_state_by_hash(
        &self,
        block_hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.0.lock().unwrap().pending_state_by_hash(block_hash)
    }

    fn maybe_pending(
        &self,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.0.lock().unwrap().maybe_pending()
    }
}

pub(crate) async fn wait_for_state(
    watch: &mut outbe_tee::TeeAuthorityWatch,
    expected: outbe_tee::TeeAuthorityState,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while watch.snapshot() != expected {
            watch.changed().await.unwrap();
        }
    })
    .await
    .expect("authority did not reach the expected state within five seconds");
}
