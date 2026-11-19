use alloy_primitives::B256;
use outbe_tee::{
    dcap_protocol::DcapOnboardingContextV1,
    finalized_admission::{
        MAX_COMMITTEE_TRANSITION_RECORD_BYTES, MAX_FINALIZED_ADMISSION_RECORD_BYTES,
    },
    upgrade_transfer::{UpgradeKeyProofV1, MAX_UPGRADE_COMMITTEES},
};

fn context() -> DcapOnboardingContextV1 {
    outbe_tee::test_utils::onboarding_context_fixture(
        [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19],
        20,
        21,
    )
}

fn proof() -> UpgradeKeyProofV1 {
    UpgradeKeyProofV1 {
        anchor_outcome: vec![0xa1, 0xa2].into(),
        committee_transitions: vec![vec![0xb1, 0xb2].into()],
        admission: vec![0xc1, 0xc2].into(),
    }
}

#[test]
fn proof_validation_preserves_dimension_record_and_aggregate_error_order() {
    let mut p = proof();
    p.anchor_outcome = Vec::<u8>::new().into();
    p.committee_transitions = vec![Vec::<u8>::new().into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    p = proof();
    p.committee_transitions = vec![Vec::<u8>::new().into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: invalid upgrade committee record size"
    );
    p = proof();
    p.committee_transitions = vec![Vec::<u8>::new().into(); MAX_UPGRADE_COMMITTEES + 1];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof dimensions exceed limits"
    );
    p = proof();
    p.committee_transitions = vec![vec![0xb1; MAX_COMMITTEE_TRANSITION_RECORD_BYTES + 1].into()];
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: invalid upgrade committee record size"
    );
    p = proof();
    p.committee_transitions = vec![vec![0xb1; MAX_COMMITTEE_TRANSITION_RECORD_BYTES].into()];
    p.admission = vec![0xc1; MAX_FINALIZED_ADMISSION_RECORD_BYTES].into();
    assert_eq!(
        p.validate().unwrap_err().to_string(),
        "enclave returned error: upgrade proof exceeds aggregate limit"
    );
}

use outbe_tee::test_utils::resident_context;

#[test]
fn resident_upgrade_export_checks_proof_and_keeps_donor_material() {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::{
        resident::ResidentOfferMaterial, upgrade_transfer::export_from_network_source,
    };
    let binding = NetworkBindingV1 {
        chain_id: context().chain_id,
        genesis_hash: context().genesis_hash,
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let donor = std::sync::Arc::new(resident_context(7, binding));
    donor
        .activate_for_test(
            ResidentOfferMaterial::for_test(vec![5; 96], B256::from(binding.chain_id), 20, 21)
                .unwrap(),
        )
        .unwrap();
    let public = donor.network_encryption_public().unwrap();
    let mut recipient = context();
    recipient.network_encryption_public = public;
    let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
        std::sync::Arc::clone(&donor),
    ));
    // Even an installed resident donor must not turn malformed finalized proof into an export.
    assert!(export_from_network_source(recipient, &proof(), false).is_err());
    assert_eq!(donor.network_encryption_public().unwrap(), public);
    let exported = export_from_network_source(recipient, &proof(), true).unwrap();
    assert_eq!(
        exported,
        export_from_network_source(recipient, &proof(), true).unwrap()
    );
    let artifact =
        outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(&exported).unwrap();
    assert_eq!(artifact.context, recipient);
    assert!(!artifact.ciphertext.is_empty());
    assert_eq!(donor.network_encryption_public().unwrap(), public);
}
