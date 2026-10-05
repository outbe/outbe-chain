#![cfg(feature = "tee-attestation-v1")]

use alloy_primitives::{b256, B256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, SigningKey};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, CodecError,
    DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1,
    NetworkBindingV1, NodeHostAuthorizationWitnessV1, NodeIdV1, PlatformTcbStatusSetV1,
    QvlTcbStatusV1, RegistrationIntentV1, RegistryMutatorV1, ResourceScheduleV1,
    SystemGasScheduleV1, TeeBootstrapGasInputV1, TeeMeasurementRuleV1, TeePolicyScheduleEntryV1,
    TeePolicyScheduleV1, TeePolicyV1, TeeRegistryGasScheduleV1, TransitionKeyReadyProofV1,
    TrustedNetworkDescriptorV1, ACTIVE_TEE_ATTESTATION_V1_MANIFEST, MAX_ACTIVE_MEASUREMENT_RULES,
    MAX_ATTESTATION_EVIDENCE_BYTES, MAX_COLLATERAL_COMPONENT_BYTES,
    MAX_EVIDENCE_CALL_FRAMING_BYTES, MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES, MAX_QUOTE_BYTES,
    MAX_TEE_BOOTSTRAP_BYTES,
};

fn validator_intent(genesis_hash: B256) -> RegistrationIntentV1 {
    let node_id = node_id(&SigningKey::from_bytes((&[0x31; 32]).into()).unwrap());
    RegistrationIntentV1 {
        chain_id: [0; 32],
        genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x21),
        node_id,
        enclave_id: B256::repeat_byte(0x41),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
        node_host_authorization_hash: B256::repeat_byte(0x54),
    }
}

fn node_id(key: &SigningKey) -> NodeIdV1 {
    NodeIdV1 {
        reth_p2p_public: key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    }
}

fn recoverable_signature(key: &SigningKey, prehash: B256) -> [u8; 65] {
    let (signature, recovery_id) = key.sign_prehash(prehash.as_slice()).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(signature.to_bytes().as_slice());
    out[64] = recovery_id.to_byte();
    out
}

fn validator_initialization_manifest(key: &SigningKey) -> EnclaveInitializationManifestV1 {
    EnclaveInitializationManifestV1 {
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x11),
        attestation_mode: AttestationMode::DcapRequired,
        node_id: node_id(key),
        initialization_challenge: [0x41; 32],
        node_host_noise_x25519: [0x42; 32],
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
    }
}

fn intent_for_manifest(manifest: &EnclaveInitializationManifestV1) -> RegistrationIntentV1 {
    RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: manifest.attestation_mode,
        policy_hash: B256::repeat_byte(0x21),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    }
}

#[path = "tee_attestation_v1/evidence.rs"]
mod evidence;
#[path = "tee_attestation_v1/gas.rs"]
mod gas;
#[path = "tee_attestation_v1/network.rs"]
mod network;
#[path = "tee_attestation_v1/policy.rs"]
mod policy;
