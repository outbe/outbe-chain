use crate::lifecycle::test_finality::{CertifiedBlockFixture, FinalityCommitteeFixture};
use alloy_primitives::{B256, U256};

use commonware_consensus::types::Epoch;

use crate::{
    dcap_protocol::DcapOnboardingContextV1,
    finalized_admission::*,
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, tee_attestation_v1::*};

pub fn context(seed: u8, descriptor: &TrustedNetworkDescriptorV1) -> InProcessTeeContext {
    let mut manifest = super::support::context(seed).manifest().clone();
    manifest.chain_id = descriptor.network_binding.chain_id;
    manifest.genesis_hash = descriptor.network_binding.genesis_hash;
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    InProcessTeeContext::new(
        ResidentTeeKeys::from_seed([seed; 32]).unwrap(),
        identity,
        manifest,
        descriptor.clone(),
        ProofSourceV1::NoHardware,
    )
    .unwrap()
}
fn compact(block: &CertifiedBlockFixture) -> CertifiedHeaderV1 {
    let decoded = outbe_simplex::committee_chain::decode_public_finalized_block(
        &block.finalization,
        &block.block,
        outbe_simplex::bls::MAX_COMMITTEE_MEMBERS,
    )
    .unwrap();
    CertifiedHeaderV1 {
        finalization: block.finalization.clone(),
        header: alloy_rlp::encode(decoded.block.header()),
        descendants: Vec::new(),
    }
}
pub struct AdmissionFixture {
    pub anchor: Vec<u8>,
    pub transitions: Vec<Vec<u8>>,
    pub proof: FinalizedAdmissionWitnessV1,
}
impl AdmissionFixture {
    pub fn descriptor(genesis: &FinalityCommitteeFixture) -> TrustedNetworkDescriptorV1 {
        TrustedNetworkDescriptorV1 {
            network_binding: NetworkBindingV1 {
                chain_id: U256::from(676_u64).to_be_bytes(),
                genesis_hash: B256::repeat_byte(0x31),
                attestation_mode: AttestationMode::NoAttestationDev,
            },
            genesis_consensus_keys: genesis.public_keys_min_pk(),
        }
    }
    pub fn new(genesis: &FinalityCommitteeFixture, context: &DcapOnboardingContextV1) -> Self {
        let (state_root, registry_account, registry_storage) = registry_opening(context);
        let successor = FinalityCommitteeFixture::new(50);
        let first = genesis.certify_block(
            Epoch::new(0),
            1,
            900,
            B256::repeat_byte(0x81),
            successor.preannounce_extra_data(Epoch::new(1)),
        );
        let second = successor.certify_block(Epoch::new(1), 2, 1000, state_root, vec![]);
        let child = successor.certify_child_block(super::finality::CertifiedChildBlockInput {
            epoch: Epoch::new(1),
            height: 3,
            timestamp: 1001,
            state_root,
            extra_data: vec![],
            parent_hash: second.block_hash,
        });
        let mut admission = compact(&second);
        admission.finalization = child.finalization.clone();
        admission.descendants.push(compact(&child).header);
        Self {
            anchor: genesis.outcome(Epoch::new(0)),
            transitions: vec![compact(&first).encode_canonical().unwrap()],
            proof: FinalizedAdmissionWitnessV1 {
                admission,
                registry_account,
                registry_storage,
            },
        }
    }
}

fn registry_opening(
    c: &DcapOnboardingContextV1,
) -> (B256, MptAccountProofV1, Vec<MptStorageProofV1>) {
    use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
    let claims = [
        U256::from_be_bytes(c.network_encryption_public),
        U256::from(c.key_epoch),
        U256::from(c.network_encryption_epoch),
        U256::from_be_bytes(c.context_hash().0),
        U256::from(10000),
        U256::from(123),
        U256::from(123),
        U256::from_be_bytes(c.policy_hash.0),
        U256::from(1),
    ];
    let slots = upgrade_registry_slots_v1(c);
    let leaves: Vec<_> = slots
        .iter()
        .zip(claims)
        .map(|(key, value)| (U256::from_be_bytes(key.0), value))
        .collect();
    let (root, nodes) = storage_trie(&leaves);
    let account = reth_trie::TrieAccount {
        storage_root: root,
        nonce: 1,
        balance: U256::ZERO,
        code_hash: B256::repeat_byte(0x91),
    };
    let (state_root, accounts) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
    let opening = MptAccountProofV1 {
        nonce: 1,
        balance: U256::ZERO,
        code_hash: account.code_hash,
        storage_root: root,
        nodes: accounts[&TEE_REGISTRY_ADDRESS]
            .iter()
            .map(|n| n.to_vec())
            .collect(),
    };
    let storage = slots
        .into_iter()
        .zip(claims)
        .zip(nodes)
        .map(|((key, value), proof)| MptStorageProofV1 {
            key,
            value,
            nodes: proof.into_iter().map(|n| n.to_vec()).collect(),
        })
        .collect();
    (state_root, opening, storage)
}
