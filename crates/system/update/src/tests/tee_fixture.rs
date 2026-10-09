use super::TEST_CHAIN_ID;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{AttestationMode, TeeMeasurementRuleV1, TeePolicyV1};
use outbe_primitives::tee_test_utils::gramine_direct_policy_v1;

/// The first policy of a chain: version 1, active from height 1, with no
/// predecessor.
pub(super) fn initial_tee_policy(
    genesis_hash: B256,
    mrenclave: B256,
    material_seed: u8,
) -> TeePolicyV1 {
    policy_fixture(genesis_hash, (1, 1, B256::ZERO), mrenclave, material_seed)
}

/// The version 2 successor of `current`, active from `activation_height`.
pub(super) fn successor_tee_policy(
    current: &TeePolicyV1,
    activation_height: u64,
    mrenclave: B256,
    material_seed: u8,
) -> TeePolicyV1 {
    policy_fixture(
        current.genesis_hash,
        (2, activation_height, current.policy_hash().unwrap()),
        mrenclave,
        material_seed,
    )
}

/// A DCAP policy with the Intel DCAP v3 header of the shared test policy and
/// `(policy_version, activation_height, predecessor_policy_hash)`.
fn policy_fixture(
    genesis_hash: B256,
    (policy_version, activation_height, predecessor_policy_hash): (u64, u64, B256),
    mrenclave: B256,
    material_seed: u8,
) -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version,
        chain_id: alloy_primitives::U256::from(TEST_CHAIN_ID).to_be_bytes(),
        genesis_hash,
        activation_height,
        predecessor_policy_hash,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(material_seed),
        resource_schedule_hash: B256::repeat_byte(material_seed + 1),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave,
            mrsigner: B256::repeat_byte(material_seed + 3),
            isv_prod_id: 7,
            minimum_isv_svn: 2,
            admit_from_height: activation_height,
            admit_until_height_exclusive: u64::MAX,
        }],
        ..gramine_direct_policy_v1(TEST_CHAIN_ID, genesis_hash).unwrap()
    }
}
