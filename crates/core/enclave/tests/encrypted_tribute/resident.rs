use alloy_primitives::{B256, U256};
use outbe_enclave::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_primitives::tee_attestation_v1::*;
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    protocol::PayloadLane,
    resident::{ResidentOfferMaterial, ResidentTeeKeys, StateKeyHandle},
    NodeHostIdentityV1, TransportError,
};

pub fn context(chain: u64) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([7; 32]).unwrap();
    let node = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(chain).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
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
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: Vec::new(),
    };
    let context = InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        descriptor,
        ProofSourceV1::NoHardware,
    )
    .unwrap();
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(vec![5; 96], B256::from(U256::from(chain)), 0, 0)
                .unwrap(),
        )
        .unwrap();
    context
}

pub fn state(
    context: &InProcessTeeContext,
    derive: fn(&ResidentOfferMaterial, B256, u64) -> outbe_enclave::errors::Result<StateKeyHandle>,
) -> StateKeyHandle {
    context
        .with_offer_material(|material| {
            derive(material, B256::from(context.manifest().chain_id), 0)
        })
        .unwrap()
        .unwrap()
}

pub fn call(
    context: &InProcessTeeContext,
    request: &EnclaveRequest,
) -> Result<EnclaveResponse, TransportError> {
    let lane = if matches!(request, EnclaveRequest::DeriveAccountKeys { .. }) {
        PayloadLane::AccountQuery
    } else {
        PayloadLane::Execution
    };
    let wire = request.encode_wire(lane)?;
    let outbe_tee::protocol::EnclaveRequest::Payload { lane, payload } = wire else {
        panic!("domain request must encode a payload")
    };
    let response =
        outbe_enclave::host::domain_in_process::dispatch_payload(context, lane, &payload)?;
    EnclaveResponse::from_wire_response(&response)
}
