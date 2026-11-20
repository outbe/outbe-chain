//! Shared unit-test resident identity; not compiled into the production API.
use super::*;
use crate::{
    resident::{ResidentOfferMaterial, ResidentTeeKeys},
    NodeHostIdentityV1,
};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::*;

pub(super) fn context(mode: AttestationMode, source: ProofSourceV1) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([9; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: [1; 32],
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: mode,
        node_id: NodeIdV1 {
            reth_p2p_public: node_key()
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
        genesis_consensus_keys: Vec::new(),
    };
    let identity = NodeHostIdentityV1 {
        network_binding: descriptor.network_binding,
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    InProcessTeeContext::new(keys, identity, manifest, descriptor, source).unwrap()
}
pub(super) fn node_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap()
}
pub(super) fn material(context: &InProcessTeeContext, byte: u8) -> ResidentOfferMaterial {
    ResidentOfferMaterial::for_test(
        vec![byte; 96],
        B256::from(context.manifest().chain_id),
        0,
        0,
    )
    .unwrap()
}
pub(super) fn registration(context: &InProcessTeeContext) -> RegistrationIntentV1 {
    let manifest = context.manifest();
    RegistrationIntentV1 {
        // Fixed operation counters and policy are independent of the resident identity.
        operation: AttestationOperationV1::RegisterEnclave,
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 2000,
        policy_hash: B256::repeat_byte(0x41),
        binding_id: B256::repeat_byte(0x42),
        // Only these fields bind the local resident recipient and NodeHost.
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        enclave_id: manifest.enclave_id().unwrap(),
        node_id: manifest.node_id.clone(),
        attestation_mode: manifest.attestation_mode,
        genesis_hash: manifest.genesis_hash,
        chain_id: manifest.chain_id,
    }
}
