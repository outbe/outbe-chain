//! Shared deterministic fixture for DCAP onboarding records.
use crate::dcap_protocol::DcapOnboardingContextV1;
use alloy_primitives::B256;

pub fn onboarding_context_fixture(
    seeds: [u8; 9],
    key_epoch: u64,
    network_encryption_epoch: u64,
) -> DcapOnboardingContextV1 {
    let [chain, genesis, intent, node, enclave, binding, policy, recipient, offer] = seeds;
    DcapOnboardingContextV1 {
        chain_id: [chain; 32],
        genesis_hash: B256::repeat_byte(genesis),
        intent_hash: B256::repeat_byte(intent),
        node_id_hash: B256::repeat_byte(node),
        enclave_id: B256::repeat_byte(enclave),
        binding_id: B256::repeat_byte(binding),
        policy_hash: B256::repeat_byte(policy),
        recipient_x25519: [recipient; 32],
        network_encryption_public: [offer; 32],
        key_epoch,
        network_encryption_epoch,
    }
}
