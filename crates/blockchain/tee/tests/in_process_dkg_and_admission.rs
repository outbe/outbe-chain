//! RAM-only DKG and genesis-anchored admission contracts.
use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_tee::in_process::{InProcessError, ProofSourceV1};

#[test]
fn dcap_without_hardware_fails_before_participation() {
    assert!(matches!(
        ProofSourceV1::NoHardware.ensure_available(AttestationMode::DcapRequired),
        Err(InProcessError::HardwareProofUnavailable)
    ));
    assert!(ProofSourceV1::NoHardware
        .ensure_available(AttestationMode::NoAttestationDev)
        .is_ok());
    use outbe_primitives::tee_attestation_v1::{NetworkBindingV1, TrustedNetworkDescriptorV1};
    use outbe_tee::{
        in_process::InProcessTeeContext, resident::ResidentTeeKeys, NodeHostIdentityV1,
    };
    let mut descriptor = TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(676).to_be_bytes(),
            genesis_hash: alloy_primitives::B256::repeat_byte(0x31),
            attestation_mode: AttestationMode::NoAttestationDev,
        },
        genesis_consensus_keys: Vec::new(),
    };
    let initialized = fixtures::context(99, &descriptor);
    let mut manifest = initialized.manifest().clone();
    manifest.attestation_mode = AttestationMode::DcapRequired;
    descriptor.network_binding = manifest.network_binding();
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    assert!(matches!(
        InProcessTeeContext::new(
            ResidentTeeKeys::from_seed([99; 32]).unwrap(),
            identity,
            manifest,
            descriptor,
            ProofSourceV1::NoHardware
        ),
        Err(InProcessError::HardwareProofUnavailable)
    ));
}

#[path = "in_process_dkg_and_admission/finality.rs"]
mod finality;
#[path = "in_process_dkg_and_admission/fixtures.rs"]
mod fixtures;

#[test]
fn four_party_dkg_finalized_onboarding_and_upgrade_are_ram_only() {
    use ed25519_dalek::Verifier as _;
    use fixtures::*;
    use outbe_primitives::tee_attestation_v1::AttestationOperationV1;
    use outbe_tee::dcap_protocol::DcapOnboardingArtifactV1;

    outbe_simplex::config::init_consensus_chain_id(676).unwrap();
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = AdmissionFixture::descriptor(&genesis);
    let founders = (1..=4)
        .map(|seed| context(seed, &descriptor))
        .collect::<Vec<_>>();
    let public = run_dkg(&founders);
    let recipient = context(5, &descriptor);
    assert!(!recipient.offer_key_ready().unwrap());
    let registration = intent(&recipient, AttestationOperationV1::RegisterEnclave);
    let onboarding = onboarding_context(&registration, public);
    let artifact = founders[0]
        .prepare_no_attestation_dev_onboarding_artifact_v2(onboarding)
        .unwrap();
    let repeated = founders[1]
        .prepare_no_attestation_dev_onboarding_artifact_v2(onboarding)
        .unwrap();
    assert_eq!(artifact.onboarding_artifact, repeated.onboarding_artifact);
    let preimage = outbe_tee::tee_call::canonical_attestation_preimage(
        b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
        artifact.request_hash,
        &artifact.onboarding_artifact,
    )
    .unwrap();
    ed25519_dalek::VerifyingKey::from_bytes(&founders[0].attestation_public())
        .unwrap()
        .verify(
            &preimage,
            &ed25519_dalek::Signature::from_bytes(&artifact.attestation_tag),
        )
        .unwrap();
    let fixture = AdmissionFixture::new(&genesis, &onboarding, false);
    let witness = fixture.proof.encode_canonical().unwrap();
    let authorization = recipient.authorization_witness().unwrap();

    // The host cannot replace authenticated MPT nodes or BLS finalizations.
    let mut bad_mpt = fixture.proof.clone();
    bad_mpt.registry_account.nodes[0][0] ^= 1;
    let mut bad_storage = fixture.proof.clone();
    bad_storage.registry_storage[0].nodes[0][0] ^= 1;
    let mut bad_bls = fixture.proof.clone();
    let last = bad_bls.admission.finalization.len() - 1;
    bad_bls.admission.finalization[last] ^= 1;
    let mut bad_ancestry = fixture.proof.clone();
    bad_ancestry.admission.descendants.clear();
    for forged in [bad_mpt, bad_storage, bad_bls, bad_ancestry] {
        let bytes = forged.encode_canonical().unwrap();
        assert!(recipient
            .ingest_finalized_onboarding_v1(fixture.request(
                (&artifact.onboarding_artifact, &bytes),
                &registration,
                &authorization,
                &onboarding
            ))
            .is_err());
        assert!(!recipient.offer_key_ready().unwrap());
        assert_eq!(recipient.admission_anchor().unwrap(), None);
    }
    // A proof from another committee cannot skip the genesis-authenticated transition.
    let mut request = fixture.request(
        (&artifact.onboarding_artifact, &witness),
        &registration,
        &authorization,
        &onboarding,
    );
    request.committee_transitions = &[];
    assert!(recipient.ingest_finalized_onboarding_v1(request).is_err());

    let mut wrong_host = authorization.clone();
    wrong_host.node_host_noise_x25519[0] ^= 1;
    assert!(recipient
        .ingest_finalized_onboarding_v1(fixture.request(
            (&artifact.onboarding_artifact, &witness),
            &registration,
            &wrong_host,
            &onboarding
        ))
        .is_err());
    let mut wrong_intent = registration.clone();
    wrong_intent.registration_version += 1;
    assert!(recipient
        .ingest_finalized_onboarding_v1(fixture.request(
            (&artifact.onboarding_artifact, &witness),
            &wrong_intent,
            &authorization,
            &onboarding
        ))
        .is_err());
    let wrong_enclave = context(6, &descriptor);
    assert!(wrong_enclave
        .ingest_finalized_onboarding_v1(fixture.request(
            (&artifact.onboarding_artifact, &witness),
            &registration,
            &authorization,
            &onboarding
        ))
        .is_err());
    assert!(!wrong_enclave.offer_key_ready().unwrap());
    let mut corrupted =
        DcapOnboardingArtifactV1::decode_canonical(&artifact.onboarding_artifact).unwrap();
    corrupted.ciphertext[0] ^= 1;
    assert!(recipient
        .ingest_finalized_onboarding_v1(fixture.request(
            (&corrupted.encode_canonical().unwrap(), &witness),
            &registration,
            &authorization,
            &onboarding
        ))
        .is_err());
    let anchor = recipient
        .ingest_finalized_onboarding_v1(fixture.request(
            (&artifact.onboarding_artifact, &witness),
            &registration,
            &authorization,
            &onboarding,
        ))
        .unwrap();
    assert_eq!(anchor.block_number, 2);
    assert_eq!(recipient.network_encryption_public().unwrap(), public);
    assert_eq!(recipient.admission_anchor().unwrap(), Some(anchor));
    assert_eq!(format!("{recipient:?}"), "InProcessTeeContext([REDACTED])");

    // Domain-neutral handles operate identically, without a raw key getter.
    let message = b"resident state";
    let nonce = [7; 12];
    let donor_state = founders[0].state_key_handle(b"outbe/state/v1/", 0).unwrap();
    let recipient_state = recipient.state_key_handle(b"outbe/state/v1/", 0).unwrap();
    let ciphertext = donor_state.aead_encrypt(&nonce, message).unwrap();
    assert_eq!(
        recipient_state
            .aead_decrypt(&nonce, &ciphertext)
            .unwrap()
            .as_slice(),
        message
    );

    let candidate = context(7, &descriptor);
    let transition = intent(
        &candidate,
        AttestationOperationV1::TransitionEnclaveMeasurement,
    );
    let upgrade = onboarding_context(&transition, public);
    let upgrade_fixture = AdmissionFixture::new(&genesis, &upgrade, true);
    let upgrade_witness = upgrade_fixture.proof.encode_canonical().unwrap();
    let candidate_auth = candidate.authorization_witness().unwrap();
    // An export template contains no secret; the donor alone fills its ciphertext.
    let template = DcapOnboardingArtifactV1 {
        context: upgrade,
        nonce: [0; 12],
        ciphertext: vec![0; 16],
    }
    .encode_canonical()
    .unwrap();
    assert!(founders[0]
        .export_upgrade_key_v1(upgrade_fixture.request(
            (&template, &witness),
            &transition,
            &candidate_auth,
            &upgrade
        ))
        .is_err());
    let sealed = founders[0]
        .export_upgrade_key_v1(upgrade_fixture.request(
            (&template, &upgrade_witness),
            &transition,
            &candidate_auth,
            &upgrade,
        ))
        .unwrap()
        .encode_canonical()
        .unwrap();
    candidate
        .ingest_upgrade_key_v1(upgrade_fixture.request(
            (&sealed, &upgrade_witness),
            &transition,
            &candidate_auth,
            &upgrade,
        ))
        .unwrap();
    assert_eq!(candidate.network_encryption_public().unwrap(), public);
    let ready = candidate
        .transition_key_ready_proof_v1(&transition)
        .unwrap();
    assert_eq!(ready.resident_offer_public, public);
    assert!(ready.verify_for_transition(&transition, public).is_ok());
}
