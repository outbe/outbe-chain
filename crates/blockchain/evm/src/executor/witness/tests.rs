use super::*;
use ed25519_dalek::{Signer, SigningKey};
use outbe_primitives::reshare_artifact::OutbeBlockArtifacts;
use outbe_tee::{
    protocol::EnclaveResponse,
    witness::{
        consensus_call, decode_witness_payload_v1, encode_witness_payload_v1, EnclaveWitnessEntryV1,
    },
    TransportError,
};
use reth_ethereum::node::api::ConfigureEvm;

fn sign_nod_read(key: &SigningKey, inputs_hash: B256, amount: U256) -> EnclaveResponse {
    let preimage = outbe_tee::nod_mine::nod_read_preimage(inputs_hash, amount);
    let sig = key.sign(&preimage);
    EnclaveResponse::NodAmountReadV1 {
        amount,
        inputs_canonical_hash: inputs_hash,
        attestation_tag: sig.to_bytes().to_vec(),
    }
}

fn verify_nod_read(
    pub_key: &[u8; 32],
    expected_hash: B256,
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    let EnclaveResponse::NodAmountReadV1 {
        amount,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if *inputs_canonical_hash != expected_hash {
        return Err(TransportError::Attestation("hash mismatch".into()));
    }
    let preimage = outbe_tee::nod_mine::nod_read_preimage(expected_hash, *amount);
    outbe_tee::tribute_v2::verify_attestation(pub_key, &preimage, attestation_tag)
}

#[test]
fn reuses_prewarm_results_is_pinned_false() {
    let config = crate::config::OutbeEvmConfig::new(Arc::new(ChainSpec::default()));
    assert!(
        !config.reuses_prewarm_results(),
        "OutbeEvmConfig must never reuse speculative prewarm results without witness commit conditions"
    );
}

#[test]
fn block_one_and_mandatory_prefix_worst_case_fixture_fits_budget() {
    let leader_sk = SigningKey::from_bytes(&[0x11; 32]);
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        timestamp_millis_part: 500,
        ..Default::default()
    })
    .unwrap();

    let mut tape = EnclaveWitnessTapeV1::default();
    for idx in 0..8u8 {
        let resp = sign_nod_read(
            &leader_sk,
            B256::repeat_byte(idx.wrapping_add(1)),
            U256::from(1000u64 + u64::from(idx)),
        );
        tape.prefix
            .push(EnclaveWitnessEntryV1::try_from(resp).unwrap());
    }
    let mut bootstrap_entries = Vec::new();
    for idx in 0..16u8 {
        let request_hash = B256::repeat_byte(0x80 | idx);
        let outcome = vec![0xAA; 96];
        let onboarding_artifact = vec![0xBB; 320];
        let preimage = outbe_tee::dcap_protocol::dcap_onboarding_attestation_preimage(
            request_hash,
            &outcome,
            &onboarding_artifact,
        )
        .unwrap();
        let sig = leader_sk.sign(&preimage);
        bootstrap_entries.push(
            EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
                request_hash,
                outcome,
                onboarding_artifact,
                attestation_tag: sig.to_bytes().to_vec(),
            },
        );
    }
    tape.txs.push(TxWitnessV1 {
        tx_index: 0,
        entries: bootstrap_entries,
    });

    let raw = postcard::to_allocvec(&tape).unwrap();
    assert!(raw.len() <= MAX_DECOMPRESSED_WITNESS_BYTES);
    assert!(tape_fits_budget(&tape, &base_extra));
    let encoded = encode_witness_payload_v1(&tape).unwrap();
    assert!(encoded.len().saturating_add(3) <= ENCLAVE_WITNESS_RESERVED_BYTES);
    let decoded = decode_witness_payload_v1(&encoded).unwrap();
    assert_eq!(decoded, tape);
}

#[test]
fn proposer_record_verifier_with_enclave_and_verifier_replay_roundtrip() {
    let leader_sk = SigningKey::from_bytes(&[0x21; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let validator_sk = SigningKey::from_bytes(&[0x22; 32]);
    let validator_pk = validator_sk.verifying_key().to_bytes();
    assert_ne!(leader_pk, validator_pk);

    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
    let prefix_hash = B256::repeat_byte(0x31);
    let tx_hash = B256::repeat_byte(0x32);

    // 1. ProposerRecord
    let proposer_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader_pk),
        EnclaveWitnessTapeV1::default(),
    )));
    {
        let _scope = WitnessExecutionScope::enter(proposer_state.clone());
        let (_, resp) = consensus_call(
            "prefix_read",
            || {
                Ok((
                    leader_pk,
                    sign_nod_read(&leader_sk, prefix_hash, U256::from(42)),
                ))
            },
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
        assert!(
            matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(42))
        );
    }
    {
        let mut guard = proposer_state.lock().unwrap();
        let staged = std::mem::take(&mut guard.staged);
        guard.tape.prefix.extend(staged);
        guard.tx_index = Some(2);
        guard.is_user_candidate = true;
    }
    {
        let _scope = WitnessExecutionScope::enter(proposer_state.clone());
        consensus_call(
            "tx_read",
            || {
                Ok((
                    leader_pk,
                    sign_nod_read(&leader_sk, tx_hash, U256::from(99)),
                ))
            },
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&proposer_state),
        2,
        &base_extra,
        true,
    ));
    let encoded_payload = {
        let mut guard = proposer_state.lock().unwrap();
        let entries = std::mem::take(&mut guard.staged);
        guard.tape.txs.push(TxWitnessV1 {
            tx_index: 2,
            entries,
        });
        encode_witness_payload_v1(&guard.tape).unwrap()
    };

    // 2. VerifierWithEnclave (validator with distinct enclave key)
    let decoded_tape = decode_witness_payload_v1(&encoded_payload).unwrap();
    let verifier_enclave_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader_pk),
        decoded_tape.clone(),
    )));
    {
        let _scope = WitnessExecutionScope::enter(verifier_enclave_state.clone());
        consensus_call(
            "prefix_read",
            || {
                Ok((
                    validator_pk,
                    sign_nod_read(&validator_sk, prefix_hash, U256::from(42)),
                ))
            },
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
    }
    {
        let mut guard = verifier_enclave_state.lock().unwrap();
        assert_eq!(guard.prefix_cursor, guard.tape.prefix.len());
        guard.tx_index = Some(2);
        guard.replay_cursor = 0;
    }
    {
        let _scope = WitnessExecutionScope::enter(verifier_enclave_state.clone());
        consensus_call(
            "tx_read",
            || {
                Ok((
                    validator_pk,
                    sign_nod_read(&validator_sk, tx_hash, U256::from(99)),
                ))
            },
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&verifier_enclave_state),
        2,
        &base_extra,
        false,
    ));

    // 3. VerifierReplay (keyless fullnode: transport closure must never execute)
    let replay_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader_pk),
        decoded_tape,
    )));
    {
        let _scope = WitnessExecutionScope::enter(replay_state.clone());
        consensus_call(
            "prefix_read",
            || panic!("VerifierReplay must never call local transport"),
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
    }
    {
        let mut guard = replay_state.lock().unwrap();
        guard.tx_index = Some(2);
        guard.replay_cursor = 0;
    }
    {
        let _scope = WitnessExecutionScope::enter(replay_state.clone());
        let (_, resp) = consensus_call(
            "tx_read",
            || panic!("VerifierReplay must never call local transport"),
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
        assert!(
            matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(99))
        );
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&replay_state),
        2,
        &base_extra,
        false,
    ));
}

#[test]
fn user_candidate_exceeding_witness_budget_sets_budget_deferred_and_rolls_back() {
    let leader_sk = SigningKey::from_bytes(&[0x41; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();

    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader_pk),
        EnclaveWitnessTapeV1::default(),
    )));
    {
        let mut guard = state.lock().unwrap();
        guard.tx_index = Some(0);
        guard.is_user_candidate = true;
        // Stage an entry whose uncompressed size exceeds MAX_DECOMPRESSED_WITNESS_BYTES (262_144)
        guard.staged.push(
            EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
                request_hash: B256::repeat_byte(0x55),
                outcome: vec![0xAA; 150_000],
                onboarding_artifact: vec![0xBB; 150_000],
                attestation_tag: vec![0xCC; 64],
            },
        );
    }
    assert!(
        !OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
            Some(&state),
            0,
            &base_extra,
            true,
        ),
        "oversized user candidate must be denied commit"
    );
    let guard = state.lock().unwrap();
    assert_eq!(
        guard.candidate_rejection,
        Some(CandidateRejection::BudgetDeferred)
    );
    assert_eq!(guard.first_witness_fault, None);
    drop(guard);

    let deferral_err = crate::enclave_witness_budget_deferral(B256::repeat_byte(0x99));
    let BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) =
        deferral_err
    else {
        panic!("expected InvalidTx validation error");
    };
    assert!(crate::is_enclave_witness_budget_deferral(error.as_ref()));
}

#[test]
fn verifier_unconsumed_tx_witness_entries_latch_tape_invalid_on_commit_check() {
    let leader_sk = SigningKey::from_bytes(&[0x61; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
    let resp = sign_nod_read(&leader_sk, B256::repeat_byte(0x62), U256::from(1));
    let tape = EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: vec![TxWitnessV1 {
            tx_index: 0,
            entries: vec![EnclaveWitnessEntryV1::try_from(resp).unwrap()],
        }],
    };
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader_pk),
        tape,
    )));
    assert!(
        !OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
            Some(&state),
            0,
            &base_extra,
            false,
        ),
        "unconsumed witness entry at tx_index=0 must reject commit"
    );
    assert_eq!(
        state.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

#[test]
fn witness_fault_to_block_error_preserves_typed_witness_abort_reason() {
    for (fault, expected) in [
        (
            BlockWitnessFault::TapeInvalid,
            WitnessAbortReason::TapeInvalid,
        ),
        (
            BlockWitnessFault::SemanticMismatch,
            WitnessAbortReason::SemanticMismatch,
        ),
        (
            BlockWitnessFault::LocalUnavailable,
            WitnessAbortReason::LocalUnavailable,
        ),
    ] {
        let err = witness_fault_to_block_error(fault, "test detail");
        let found = NodeLocalExecutionAbort::find(&err)
            .expect("NodeLocalExecutionAbort must be discoverable in error chain");
        assert_eq!(found.witness_reason, Some(expected));
        assert_eq!(found.reason, "test detail");
    }
}
