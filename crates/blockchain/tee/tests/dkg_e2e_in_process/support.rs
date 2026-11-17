use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NetworkBindingV1, NodeIdV1, TrustedNetworkDescriptorV1,
};
use outbe_tee::{
    in_process::{HardwareProofSourceV1, InProcessError, InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};

// Ceremony tests need an explicit capability in DCAP mode but never request a quote.
struct CeremonyProofSource;
impl HardwareProofSourceV1 for CeremonyProofSource {
    fn code_identity(&self) -> outbe_tee::in_process::Result<(B256, B256, u16, u16)> {
        Err(InProcessError::HardwareProofUnavailable)
    }
    fn quote(&self, _: [u8; 64]) -> outbe_tee::in_process::Result<Vec<u8>> {
        Err(InProcessError::HardwareProofUnavailable)
    }
}

pub fn context(seed: u8, binding: NetworkBindingV1) -> InProcessTeeContext {
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
