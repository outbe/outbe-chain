//! Reuse the existing cryptographic fixture, advancing its exact lease checkpoint.
use super::{finality, shared};
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
