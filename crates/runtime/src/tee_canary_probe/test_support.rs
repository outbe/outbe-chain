//! Local, deterministic resident context for canary and private RPC contracts.
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::*;
use outbe_tee::{in_process::*, resident::ResidentTeeKeys, NodeHostIdentityV1};
use std::sync::Arc;

pub(crate) fn context(seed: u8) -> Arc<InProcessTeeContext> {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let node_id = NodeIdV1 {
        reth_p2p_public: alloy_primitives::hex!(
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        ),
    };
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: alloy_primitives::U256::from(777u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id,
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let binding = manifest.network_binding();
    Arc::new(
        InProcessTeeContext::new(
            keys,
            NodeHostIdentityV1 {
                network_binding: binding,
                reth_p2p_public: manifest.node_id.reth_p2p_public,
            },
            manifest,
            TrustedNetworkDescriptorV1 {
                network_binding: binding,
                genesis_consensus_keys: Vec::new(),
            },
            ProofSourceV1::NoHardware,
        )
        .expect("valid local resident fixture"),
    )
}

pub(crate) fn activate(context: &InProcessTeeContext) {
    context
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
}
