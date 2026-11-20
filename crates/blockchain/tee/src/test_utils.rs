//! Shared deterministic test fixtures for TEE protocol records.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{DcapCollateralComponentV1, DcapCollateralKind};

use crate::registry_view::RenewalBindingV1;

pub struct RenewalBindingFixtureV1 {
    binding: RenewalBindingV1,
}

impl RenewalBindingFixtureV1 {
    pub fn new(seed: u8) -> Self {
        let hash = |offset| B256::repeat_byte(seed.wrapping_add(offset));
        Self {
            binding: RenewalBindingV1 {
                node_id_hash: hash(0),
                enclave_id: hash(1),
                binding_id: hash(2),
                intent_hash: hash(3),
                evidence_hash: hash(4),
                policy_hash: hash(5),
                binding_version: 0,
                registration_version: 0,
                renewal_nonce: 0,
                transition_nonce: 0,
                lease_started_at: 0,
                valid_until: 0,
                collateral_valid_until: 0,
                recipient_x25519: hash(6),
                attestation_ed25519: hash(7),
                noise_responder_x25519: hash(8),
                mrenclave: hash(9),
                mrsigner: hash(10),
                isv_prod_id: 0,
                isv_svn: 0,
                platform_tcb_status: 0,
                verdict_hash: hash(11),
                node_host_authorization_hash: hash(12),
            },
        }
    }

    pub fn versions(mut self, binding: u64, registration: u64) -> Self {
        self.binding.binding_version = binding;
        self.binding.registration_version = registration;
        self
    }

    pub fn nonces(mut self, renewal: u64, transition: u64) -> Self {
        self.binding.renewal_nonce = renewal;
        self.binding.transition_nonce = transition;
        self
    }

    pub fn lease(mut self, started_at: u64, valid_until: u64, collateral_until: u64) -> Self {
        self.binding.lease_started_at = started_at;
        self.binding.valid_until = valid_until;
        self.binding.collateral_valid_until = collateral_until;
        self
    }

    pub fn keys(mut self, seed: u8) -> Self {
        let [first, second, third] = hash_triplet(seed);
        self.binding.recipient_x25519 = first;
        self.binding.attestation_ed25519 = second;
        self.binding.noise_responder_x25519 = third;
        self
    }

    pub fn measurements(mut self, seed: u8) -> Self {
        let [first, second, third] = hash_triplet(seed);
        self.binding.mrenclave = first;
        self.binding.mrsigner = second;
        self.binding.verdict_hash = third;
        self
    }

    pub fn claims(mut self, product_id: u16, svn: u16, tcb_status: u8) -> Self {
        self.binding.isv_prod_id = product_id;
        self.binding.isv_svn = svn;
        self.binding.platform_tcb_status = tcb_status;
        self
    }

    pub fn authorization(mut self, hash: B256) -> Self {
        self.binding.node_host_authorization_hash = hash;
        self
    }

    pub fn build(self) -> RenewalBindingV1 {
        self.binding
    }
}

fn hash_triplet(seed: u8) -> [B256; 3] {
    [seed, seed.wrapping_add(1), seed.wrapping_add(2)].map(B256::repeat_byte)
}

pub use outbe_dcap::test_utils::onboarding_context_fixture;

pub fn canonical_dcap_collateral_fixture() -> Vec<DcapCollateralComponentV1> {
    (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect()
}

use crate::{
    in_process::{HardwareProofSourceV1, InProcessError, InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NetworkBindingV1, NodeIdV1, TrustedNetworkDescriptorV1,
};

// DCAP-mode tests require a capability without producing hardware evidence.
struct CeremonyProofSource;
impl HardwareProofSourceV1 for CeremonyProofSource {
    fn code_identity(&self) -> crate::in_process::Result<(B256, B256, u16, u16)> {
        Err(InProcessError::HardwareProofUnavailable)
    }
    fn quote(&self, _: [u8; 64]) -> crate::in_process::Result<Vec<u8>> {
        Err(InProcessError::HardwareProofUnavailable)
    }
}

/// Deterministic resident context for ceremony and dispatch tests.
pub fn resident_context(seed: u8, binding: NetworkBindingV1) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let node_key = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
    let reth_p2p_public = node_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: binding.chain_id,
        genesis_hash: binding.genesis_hash,
        attestation_mode: binding.attestation_mode,
        node_id: NodeIdV1 { reth_p2p_public },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    InProcessTeeContext::new(
        keys,
        NodeHostIdentityV1 {
            network_binding: binding,
            reth_p2p_public,
        },
        manifest,
        TrustedNetworkDescriptorV1 {
            network_binding: binding,
            genesis_consensus_keys: Vec::new(),
        },
        ProofSourceV1::Hardware(std::sync::Arc::new(CeremonyProofSource)),
    )
    .unwrap()
}
