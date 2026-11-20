use super::*;

#[test]
fn durable_submission_policies_keep_operation_and_possession_errors_distinct() {
    let register = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    let source = read_replacement_submission(&register.paths.replacement_submission).unwrap();
    let registered = validate_durable_replacement_submission(&register.candidate, &source).unwrap();
    assert_eq!(
        registered.operation,
        AttestationOperationV1::RegisterEnclave
    );

    let bad_node = [0; 65];
    let replacement_bad_node = ReplacementCandidateSubmissionV1::new(
        source.evidence().to_vec(),
        bad_node,
        *source.enclave_signature(),
    );
    assert_eq!(
        validate_durable_replacement_submission(&register.candidate, &replacement_bad_node)
            .unwrap_err()
            .to_string(),
        "codec error: durable replacement submission proof of possession is invalid"
    );

    let replace = replacement_fixture();
    let replace_source =
        read_replacement_submission(&replace.paths.replacement_submission).unwrap();
    let replaced =
        validate_durable_replacement_submission(&replace.candidate, &replace_source).unwrap();
    assert_eq!(
        replaced.operation,
        AttestationOperationV1::ReplaceEnclaveBinding
    );

    for disallowed in [
        AttestationOperationV1::RenewEnclave,
        AttestationOperationV1::PrepareEnclaveUpgrade,
    ] {
        let disallowed_evidence = with_operation(
            AttestationEvidenceV1::decode_canonical(source.evidence()).unwrap(),
            disallowed,
        );
        let disallowed_bad_sig = ReplacementCandidateSubmissionV1::new(
            disallowed_evidence.encode_canonical().unwrap(),
            bad_node,
            [0; 64],
        );
        assert_eq!(
            validate_durable_replacement_submission(&register.candidate, &disallowed_bad_sig)
                .unwrap_err()
                .to_string(),
            "codec error: durable submission is not an allowed registration or successor operation"
        );
    }
}

#[test]
fn missing_committed_state_precedes_bad_key_and_does_not_change_checkpoints() {
    let fixture = replacement_fixture();
    let source = read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(source.evidence()).unwrap();
    let durable_bytes = std::fs::read(&fixture.paths.replacement_submission).unwrap();
    std::fs::remove_file(&fixture.paths.manifest).unwrap();
    std::fs::write(&fixture.paths.identity_key, b"invalid key").unwrap();

    assert_eq!(
        persist_replacement_candidate_submission(
            &fixture.node_data_dir,
            &evidence,
            source.node_signature(),
            source.enclave_signature(),
        )
        .unwrap_err()
        .to_string(),
        "codec error: replacement submission requires committed and candidate NodeHost state"
    );
    assert_eq!(
        load_replacement_candidate_submission(&fixture.node_data_dir)
            .unwrap_err()
            .to_string(),
        "codec error: replacement submission reload requires committed NodeHost state"
    );
    assert_eq!(
        std::fs::read(&fixture.paths.replacement_submission).unwrap(),
        durable_bytes
    );
}

fn with_operation(
    mut evidence: AttestationEvidenceV1,
    operation: AttestationOperationV1,
) -> AttestationEvidenceV1 {
    match &mut evidence {
        AttestationEvidenceV1::Dcap(value) => value.intent.operation = operation,
        AttestationEvidenceV1::NoAttestationDev(value) => value.intent.operation = operation,
    }
    evidence
}

#[test]
fn replacement_write_path_keeps_its_operation_and_signature_errors() {
    let fixture = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    let source = read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    let durable_bytes = std::fs::read(&fixture.paths.replacement_submission).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(source.evidence()).unwrap();
    let write_error = |evidence: &AttestationEvidenceV1,
                       node_signature: &[u8; 65],
                       enclave_signature: &[u8; 64]| {
        persist_replacement_candidate_submission(
            &fixture.node_data_dir,
            evidence,
            node_signature,
            enclave_signature,
        )
        .unwrap_err()
        .to_string()
    };

    assert_eq!(
        write_error(&evidence, &[0; 65], source.enclave_signature()),
        "codec error: replacement submission node signature is invalid"
    );
    assert_eq!(
        write_error(&evidence, source.node_signature(), &[0; 64]),
        "codec error: replacement submission enclave signature is invalid"
    );
    let renewal = with_operation(
        AttestationEvidenceV1::decode_canonical(source.evidence()).unwrap(),
        AttestationOperationV1::RenewEnclave,
    );
    assert_eq!(
        write_error(
            &renewal,
            source.node_signature(),
            source.enclave_signature()
        ),
        "codec error: candidate submission is not an allowed registration or successor operation"
    );
    let durable_renewal = ReplacementCandidateSubmissionV1::new(
        renewal.encode_canonical().unwrap(),
        *source.node_signature(),
        *source.enclave_signature(),
    );
    assert_eq!(
        validate_durable_replacement_submission(&fixture.candidate, &durable_renewal)
            .unwrap_err()
            .to_string(),
        "codec error: durable submission is not an allowed registration or successor operation"
    );
    assert_eq!(
        std::fs::read(&fixture.paths.replacement_submission).unwrap(),
        durable_bytes
    );
}

#[test]
fn direct_dev_enclave_signature_mismatch_fails_operation_selection_first() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::NoAttestationDev,
    );
    let source = read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(source.evidence()).unwrap();

    assert_eq!(
        persist_replacement_candidate_submission(
            &fixture.node_data_dir,
            &evidence,
            source.node_signature(),
            &[0; 64],
        )
        .unwrap_err()
        .to_string(),
        "codec error: candidate submission is not an allowed registration or successor operation"
    );
}

#[test]
fn promotion_requires_committed_state_before_it_reads_the_key() {
    let fixture = replacement_fixture();
    std::fs::remove_file(&fixture.paths.manifest).unwrap();
    std::fs::write(&fixture.paths.identity_key, b"invalid key").unwrap();

    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization)
            .unwrap_err()
            .to_string(),
        "codec error: replacement promotion requires committed NodeHost state"
    );
}
