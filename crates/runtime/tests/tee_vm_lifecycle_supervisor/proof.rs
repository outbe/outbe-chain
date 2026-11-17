//! Reuse the existing cryptographic fixture, advancing its exact lease checkpoint.
use super::*;
use alloy_primitives::{B256, U256};
use commonware_consensus::types::Epoch;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS, tee_attestation_v1::TrustedNetworkDescriptorV1,
};
use outbe_tee::{dcap_protocol::DcapOnboardingContextV1, finalized_admission::CertifiedHeaderV1};
pub use shared::onboarding_context;
pub fn context(
    seed: u8,
    descriptor: &TrustedNetworkDescriptorV1,
) -> outbe_tee::in_process::InProcessTeeContext {
    let template = shared::context(seed, descriptor);
    let mut manifest = template.manifest().clone();
    manifest.node_host_noise_x25519 = outbe_tee::NodeHostNoiseKey::from_private([4; 32]).public();
    outbe_tee::in_process::InProcessTeeContext::new(
        outbe_tee::resident::ResidentTeeKeys::from_seed([seed; 32]).unwrap(),
        *template.identity(),
        manifest,
        descriptor.clone(),
        outbe_tee::in_process::ProofSourceV1::NoHardware,
    )
    .unwrap()
}

pub fn descriptor(genesis: &finality::FinalityCommitteeFixture) -> TrustedNetworkDescriptorV1 {
    let mut descriptor = shared::AdmissionFixture::descriptor(genesis);
    descriptor.network_binding.genesis_hash = B256::repeat_byte(0x11);
    descriptor
}

pub fn finalized_proof(
    genesis: &finality::FinalityCommitteeFixture,
    context: &DcapOnboardingContextV1,
    valid_until: u64,
    checkpoint: (u64, u64),
) -> shared::AdmissionFixture {
    use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
    let mut fixture = shared::AdmissionFixture::new(genesis, context, false);
    let lease = outbe_tee::finalized_admission::onboarding_registry_slots_v1(context)[7];
    for word in &mut fixture.proof.registry_storage {
        if word.key == lease {
            word.value = U256::from(valid_until);
        }
    }
    let leaves = fixture
        .proof
        .registry_storage
        .iter()
        .map(|word| (U256::from_be_bytes(word.key.0), word.value))
        .collect::<Vec<_>>();
    let (storage_root, proofs) = storage_trie(&leaves);
    for (word, nodes) in fixture.proof.registry_storage.iter_mut().zip(proofs) {
        word.nodes = nodes.into_iter().map(|node| node.to_vec()).collect();
    }
    let account = &mut fixture.proof.registry_account;
    account.storage_root = storage_root;
    let (state_root, accounts) = account_mpt_with_proofs(&[(
        TEE_REGISTRY_ADDRESS,
        reth_trie::TrieAccount {
            nonce: account.nonce,
            balance: account.balance,
            storage_root,
            code_hash: account.code_hash,
        },
    )]);
    account.nodes = accounts[&TEE_REGISTRY_ADDRESS]
        .iter()
        .map(|node| node.to_vec())
        .collect();
    let committee = finality::FinalityCommitteeFixture::new(50);
    let transition = genesis.certify_block(
        Epoch::new(0),
        1,
        900,
        B256::repeat_byte(0x81),
        committee.preannounce_extra_data(Epoch::new(1)),
    );
    fixture.anchor = genesis.outcome(Epoch::new(0));
    fixture.transitions = vec![compact(transition).encode_canonical().unwrap()];
    fixture.proof.admission = compact(committee.certify_block(
        Epoch::new(1),
        checkpoint.0,
        checkpoint.1,
        state_root,
        Vec::new(),
    ));

    fixture
}

fn compact(signed: finality::CertifiedBlockFixture) -> CertifiedHeaderV1 {
    let decoded = outbe_consensus::committee_chain::decode_public_finalized_block(
        &signed.finalization,
        &signed.block,
        outbe_consensus::bls::MAX_COMMITTEE_MEMBERS,
    )
    .unwrap();
    CertifiedHeaderV1 {
        finalization: signed.finalization,
        header: alloy_consensus::private::alloy_rlp::encode(decoded.block.header()),
        descendants: Vec::new(),
    }
}

/// Fault injection remains at the chain boundary, not in the production scheduler.
pub(super) struct ObservedChain {
    inner: FixtureChain,
    pub checkpoint_failure: bool,
    pub proof_failure: bool,
    pub receipt_calls: std::sync::atomic::AtomicUsize,
}
impl ObservedChain {
    pub fn new(inner: FixtureChain) -> Self {
        Self {
            inner,
            checkpoint_failure: false,
            proof_failure: false,
            receipt_calls: Default::default(),
        }
    }
}
impl std::ops::Deref for ObservedChain {
    type Target = FixtureChain;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl std::ops::DerefMut for ObservedChain {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
impl LifecycleChainPort for ObservedChain {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> chain::PortResult<LifecycleCheckpointV1> {
        if self.checkpoint_failure {
            return Err(LifecyclePortError::Source(std::io::Error::from(
                std::io::ErrorKind::TimedOut,
            )));
        }
        self.inner.finalized_checkpoint().await
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        selector: &outbe_tee::registry_view::NodeBindingSelectorV1,
    ) -> chain::PortResult<LifecycleRegistryStateV1> {
        self.inner.registry_state(at, selector).await
    }
    async fn validator_record(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<Option<outbe_primitives::validator_dto::ValidatorRecord>> {
        self.inner.validator_record(at, account).await
    }
    async fn balance(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> chain::PortResult<U256> {
        self.inner.balance(at, account).await
    }
    async fn account_nonce(
        &self,
        account: Address,
        tag: LifecycleNonceTagV1,
    ) -> chain::PortResult<u64> {
        self.inner.account_nonce(account, tag).await
    }
    async fn send_raw_transaction(&self, raw: &[u8]) -> chain::PortResult<B256> {
        self.inner.send_raw_transaction(raw).await
    }
    async fn finalized_receipt(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<Option<LifecycleReceiptV1>> {
        self.receipt_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.finalized_receipt(at, hash).await
    }
    async fn onboarding_proof(
        &self,
        at: LifecycleCheckpointV1,
        hash: B256,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        if self.proof_failure {
            return Err(LifecyclePortError::Source(std::io::Error::from(
                std::io::ErrorKind::ConnectionReset,
            )));
        }
        self.inner.onboarding_proof(at, hash).await
    }
    async fn upgrade_key(
        &self,
        at: LifecycleCheckpointV1,
        context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
        proof: &outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
    ) -> chain::PortResult<LifecycleOnboardingProofV1> {
        self.inner.upgrade_key(at, context, proof).await
    }
}
