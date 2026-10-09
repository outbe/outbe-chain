//! Shared deterministic test fixtures for TEE protocol records.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{DcapCollateralComponentV1, DcapCollateralKind};

use crate::dcap_protocol::DcapOnboardingContextV1;

pub fn onboarding_context_fixture(
    seeds: [u8; 9],
    key_epoch: u64,
    tribute_offer_epoch: u64,
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
        tribute_offer_public: [offer; 32],
        key_epoch,
        tribute_offer_epoch,
    }
}

pub fn canonical_dcap_collateral_fixture() -> Vec<DcapCollateralComponentV1> {
    (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect()
}
