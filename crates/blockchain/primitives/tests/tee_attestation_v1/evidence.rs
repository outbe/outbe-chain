#[path = "../../../../../testing/fixtures/transition_key_ready.rs"]
mod transition_key_ready;

use super::*;
use transition_key_ready::signed_transition_key_ready_proof;

fn dcap_evidence_with_component_bytes(last_component_len: usize) -> AttestationEvidenceV1 {
    let intent = validator_intent(B256::repeat_byte(0x11));
    let components = (1u8..=8)
        .map(|value| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(value).unwrap(),
            bytes: vec![value; if value == 8 { last_component_len } else { 1 }],
        })
        .collect();
    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: vec![0x61],
        components,
        transition_key_ready_proof: None,
    })
}

#[test]
fn dcap_evidence_roundtrips_and_rejects_duplicate_unknown_and_trailing_components() {
    let evidence = dcap_evidence_with_component_bytes(1);
    let encoded = evidence.encode_canonical().unwrap();
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        evidence
    );

    let mut duplicate = match evidence.clone() {
        AttestationEvidenceV1::Dcap(value) => value,
        AttestationEvidenceV1::GramineDirectDev(_)
        | AttestationEvidenceV1::SevSnp(_)
        | AttestationEvidenceV1::Tdx(_) => {
            unreachable!()
        }
    };
    duplicate.components[7].kind = DcapCollateralKind::QeIdentity;
    assert_eq!(
        AttestationEvidenceV1::Dcap(duplicate)
            .encode_canonical()
            .unwrap_err(),
        CodecError::NonCanonical("DCAP component kinds must be exactly 0x01..=0x08")
    );

    let intent_len = validator_intent(B256::repeat_byte(0x11))
        .encode_canonical()
        .unwrap()
        .len();
    let first_component_kind = 6 + 1 + 4 + intent_len + 4 + 1 + 2;
    let mut unknown = encoded.clone();
    unknown[first_component_kind] = 0xff;
    assert!(matches!(
        AttestationEvidenceV1::decode_canonical(&unknown),
        Err(CodecError::UnknownDiscriminant {
            field: "DCAP collateral component",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn transition_key_ready_proof_roundtrips_and_binds_exact_transition() {
    let attestation = ed25519_dalek::SigningKey::from_bytes(&[0x71; 32]);
    let mut intent = validator_intent(B256::repeat_byte(0x11));
    intent.chain_id = [0x10; 32];
    intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    intent.registration_version = 1;
    intent.transition_nonce = 9;
    intent.attestation_ed25519 = attestation.verifying_key().to_bytes();

    let proof = signed_transition_key_ready_proof(
        &intent,
        intent.intent_hash().unwrap(),
        B256::repeat_byte(0x72),
        [0x73; 32],
        &attestation,
    );

    let encoded = proof.encode_canonical().unwrap();
    assert_eq!(encoded.len(), TransitionKeyReadyProofV1::CANONICAL_LEN);
    assert_eq!(
        TransitionKeyReadyProofV1::decode_canonical(&encoded).unwrap(),
        proof
    );
    proof.verify_for_transition(&intent, [0x73; 32]).unwrap();

    let mut wrong_chain = intent.clone();
    wrong_chain.chain_id = [0x74; 32];
    assert!(proof
        .verify_for_transition(&wrong_chain, [0x73; 32])
        .is_err());
    let mut wrong_nonce = intent.clone();
    wrong_nonce.transition_nonce += 1;
    assert!(proof
        .verify_for_transition(&wrong_nonce, [0x73; 32])
        .is_err());
    assert!(proof.verify_for_transition(&intent, [0x75; 32]).is_err());
    let mut wrong_manifest = proof;
    wrong_manifest.candidate_manifest_hash = B256::repeat_byte(0x76);
    assert!(wrong_manifest
        .verify_for_transition(&intent, [0x73; 32])
        .is_err());
    let mut wrong_signature = proof;
    wrong_signature.candidate_attestation_signature[0] ^= 1;
    assert!(wrong_signature
        .verify_for_transition(&intent, [0x73; 32])
        .is_err());
}

#[test]
fn dcap_evidence_requires_transition_proof_only_for_transition() {
    let attestation = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let mut transition = match dcap_evidence_with_component_bytes(1) {
        AttestationEvidenceV1::Dcap(value) => value,
        AttestationEvidenceV1::GramineDirectDev(_)
        | AttestationEvidenceV1::SevSnp(_)
        | AttestationEvidenceV1::Tdx(_) => {
            unreachable!()
        }
    };
    transition.intent.chain_id = [0x10; 32];
    transition.intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    transition.intent.registration_version = 1;
    transition.intent.transition_nonce = 7;
    transition.intent.attestation_ed25519 = attestation.verifying_key().to_bytes();
    assert!(AttestationEvidenceV1::Dcap(transition.clone())
        .encode_canonical()
        .is_err());

    let proof = signed_transition_key_ready_proof(
        &transition.intent,
        transition.intent.intent_hash().unwrap(),
        B256::repeat_byte(0x78),
        [0x79; 32],
        &attestation,
    );
    transition.transition_key_ready_proof = Some(proof);
    let encoded = AttestationEvidenceV1::Dcap(transition.clone())
        .encode_canonical()
        .unwrap();
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        AttestationEvidenceV1::Dcap(transition.clone())
    );

    transition.intent.operation = AttestationOperationV1::RegisterEnclave;
    transition.intent.registration_version = 0;
    transition.intent.transition_nonce = 0;
    assert!(AttestationEvidenceV1::Dcap(transition)
        .encode_canonical()
        .is_err());
}

#[test]
fn evidence_variant_rejects_an_intent_for_another_attestation_mode() {
    let mut dcap = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut dcap else {
        unreachable!()
    };
    value.intent.attestation_mode = AttestationMode::GramineDirectDev;

    assert_eq!(
        dcap.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("DCAP evidence intent mode mismatch")
    );
}

#[test]
fn evidence_codec_enforces_cap_minus_one_cap_and_cap_plus_one() {
    let base = dcap_evidence_with_component_bytes(1);
    let base_len = base.encode_canonical().unwrap().len();
    let exact_last_len = 1 + (MAX_ATTESTATION_EVIDENCE_BYTES - base_len);

    let cap_minus_one = dcap_evidence_with_component_bytes(exact_last_len - 1);
    assert_eq!(
        cap_minus_one.encode_canonical().unwrap().len(),
        MAX_ATTESTATION_EVIDENCE_BYTES - 1
    );

    let at_cap = dcap_evidence_with_component_bytes(exact_last_len);
    let at_cap_bytes = at_cap.encode_canonical().unwrap();
    assert_eq!(at_cap_bytes.len(), MAX_ATTESTATION_EVIDENCE_BYTES);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&at_cap_bytes).unwrap(),
        at_cap
    );

    let cap_plus_one = dcap_evidence_with_component_bytes(exact_last_len + 1);
    assert!(matches!(
        cap_plus_one.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence",
            actual,
            ..
        }) if actual == MAX_ATTESTATION_EVIDENCE_BYTES + 1
    ));
}

#[test]
fn evidence_codec_checks_declared_caps_before_payload_allocation() {
    let mut oversized_declared_payload = vec![1, AttestationMode::DcapRequired as u8];
    oversized_declared_payload.extend_from_slice(
        &u32::try_from(MAX_ATTESTATION_EVIDENCE_BYTES + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert!(matches!(
        AttestationEvidenceV1::decode_canonical(&oversized_declared_payload),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence payload",
            ..
        })
    ));

    let mut oversized_quote = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut oversized_quote else {
        unreachable!()
    };
    value.quote = vec![0x61; MAX_QUOTE_BYTES + 1];
    assert!(matches!(
        oversized_quote.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "SGX quote",
            ..
        })
    ));

    let mut oversized_component = dcap_evidence_with_component_bytes(1);
    let AttestationEvidenceV1::Dcap(value) = &mut oversized_component else {
        unreachable!()
    };
    value.components[7].bytes = vec![0x62; MAX_COLLATERAL_COMPONENT_BYTES + 1];
    assert!(matches!(
        oversized_component.encode_canonical(),
        Err(CodecError::LimitExceeded {
            field: "DCAP collateral component",
            ..
        })
    ));
}

fn sev_snp_evidence() -> SevSnpEvidenceV1 {
    let mut intent = validator_intent(B256::repeat_byte(0x11));
    intent.attestation_mode = AttestationMode::SevSnpRequired;
    SevSnpEvidenceV1 {
        intent,
        report: vec![0x61; SEV_SNP_REPORT_BYTES],
        ark_der: vec![0x01; 3],
        ask_der: vec![0x02; 2],
        vcek_der: vec![0x03; 1],
        transition_key_ready_proof: None,
    }
}

#[test]
fn sev_snp_evidence_roundtrips_with_the_mode_byte_in_the_frame() {
    let evidence = AttestationEvidenceV1::SevSnp(sev_snp_evidence());

    let encoded = evidence.encode_canonical().unwrap();

    assert_eq!(encoded[..2], [1, 0x03]);
    assert_eq!(evidence.mode(), AttestationMode::SevSnpRequired);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        evidence
    );
}

#[test]
fn sev_snp_evidence_rejects_a_wrong_report_length_and_bad_certificates() {
    for len in [0, SEV_SNP_REPORT_BYTES - 1, SEV_SNP_REPORT_BYTES + 1] {
        let mut value = sev_snp_evidence();
        value.report = vec![0x61; len];
        assert_eq!(
            AttestationEvidenceV1::SevSnp(value)
                .encode_canonical()
                .unwrap_err(),
            CodecError::NonCanonical("SEV-SNP report length")
        );
    }

    let mut empty = sev_snp_evidence();
    empty.ask_der.clear();
    assert_eq!(
        AttestationEvidenceV1::SevSnp(empty)
            .encode_canonical()
            .unwrap_err(),
        CodecError::NonCanonical("empty SEV-SNP certificate")
    );

    let mut at_cap = sev_snp_evidence();
    at_cap.vcek_der = vec![0x03; MAX_SEV_SNP_CERTIFICATE_BYTES];
    let at_cap = AttestationEvidenceV1::SevSnp(at_cap);
    let encoded = at_cap.encode_canonical().unwrap();
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        at_cap
    );

    let mut over_cap = sev_snp_evidence();
    over_cap.vcek_der = vec![0x03; MAX_SEV_SNP_CERTIFICATE_BYTES + 1];
    assert!(AttestationEvidenceV1::SevSnp(over_cap)
        .encode_canonical()
        .is_err());
}

#[test]
fn sev_snp_evidence_rejects_another_mode_an_empty_certificate_and_trailing_bytes() {
    let mut other_mode = sev_snp_evidence();
    other_mode.intent.attestation_mode = AttestationMode::DcapRequired;
    assert_eq!(
        AttestationEvidenceV1::SevSnp(other_mode)
            .encode_canonical()
            .unwrap_err(),
        CodecError::NonCanonical("SEV-SNP evidence intent mode mismatch")
    );

    let evidence = sev_snp_evidence();
    let encoded = AttestationEvidenceV1::SevSnp(evidence.clone())
        .encode_canonical()
        .unwrap();

    // The frame of another mode with this payload does not decode.
    let mut as_dcap = encoded.clone();
    as_dcap[1] = 0x01;
    assert!(AttestationEvidenceV1::decode_canonical(&as_dcap).is_err());

    // A zero length of the last certificate. The presence flag follows it.
    let mut zero_vcek = encoded.clone();
    let vcek_len_at = zero_vcek.len() - 1 - evidence.vcek_der.len() - 2;
    zero_vcek.drain(vcek_len_at + 2..vcek_len_at + 2 + evidence.vcek_der.len());
    zero_vcek[vcek_len_at..vcek_len_at + 2].copy_from_slice(&0_u16.to_be_bytes());
    let payload_len = u32::from_be_bytes(zero_vcek[2..6].try_into().unwrap()) - 1;
    zero_vcek[2..6].copy_from_slice(&payload_len.to_be_bytes());
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&zero_vcek).unwrap_err(),
        CodecError::NonCanonical("empty SEV-SNP certificate")
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert!(AttestationEvidenceV1::decode_canonical(&trailing).is_err());
}

#[test]
fn sev_snp_evidence_requires_a_transition_proof_only_for_a_transition() {
    let mut value = sev_snp_evidence();
    value.intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;

    assert_eq!(
        AttestationEvidenceV1::SevSnp(value)
            .encode_canonical()
            .unwrap_err(),
        CodecError::NonCanonical("transition key-ready proof presence does not match operation")
    );
}

fn tdx_evidence() -> DcapEvidenceV1 {
    let mut intent = validator_intent(B256::repeat_byte(0x11));
    intent.attestation_mode = AttestationMode::TdxRequired;
    DcapEvidenceV1 {
        intent,
        quote: vec![0x61; 64],
        components: (1u8..=8)
            .map(|value| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(value).unwrap(),
                bytes: vec![value],
            })
            .collect(),
        transition_key_ready_proof: None,
    }
}

#[test]
fn tdx_evidence_roundtrips_with_its_own_mode_byte() {
    let evidence = AttestationEvidenceV1::Tdx(tdx_evidence());
    let encoded = evidence.encode_canonical().unwrap();

    // Frame: version, then the attestation mode.
    assert_eq!(encoded[1], 0x04);
    assert_eq!(evidence.mode(), AttestationMode::TdxRequired);
    assert_eq!(
        AttestationEvidenceV1::decode_canonical(&encoded).unwrap(),
        evidence
    );
}

#[test]
fn tdx_and_dcap_evidence_do_not_decode_as_each_other() {
    // The payload layout is the same. The mode byte of the frame and the mode
    // of the intent keep the two apart.
    let tdx = AttestationEvidenceV1::Tdx(tdx_evidence())
        .encode_canonical()
        .unwrap();
    let mut as_dcap = tdx.clone();
    as_dcap[1] = AttestationMode::DcapRequired as u8;
    assert!(AttestationEvidenceV1::decode_canonical(&as_dcap).is_err());

    let dcap = dcap_evidence_with_component_bytes(1)
        .encode_canonical()
        .unwrap();
    let mut as_tdx = dcap;
    as_tdx[1] = AttestationMode::TdxRequired as u8;
    assert!(AttestationEvidenceV1::decode_canonical(&as_tdx).is_err());

    // The intent mode must be the mode of the variant.
    let mut dcap_intent = tdx_evidence();
    dcap_intent.intent.attestation_mode = AttestationMode::DcapRequired;
    assert!(AttestationEvidenceV1::Tdx(dcap_intent)
        .encode_canonical()
        .is_err());
    assert!(AttestationEvidenceV1::Dcap(tdx_evidence())
        .encode_canonical()
        .is_err());
}

#[test]
fn tdx_evidence_keeps_the_dcap_component_rules() {
    let mut seven = tdx_evidence();
    seven.components.pop();
    assert!(AttestationEvidenceV1::Tdx(seven)
        .encode_canonical()
        .is_err());

    let mut empty_quote = tdx_evidence();
    empty_quote.quote.clear();
    assert!(AttestationEvidenceV1::Tdx(empty_quote)
        .encode_canonical()
        .is_err());
}
