use super::*;
use crate::{
    call_context::WitnessExecutionScope,
    protocol::EnclaveResponse,
    tee_call::{canonical_attestation_preimage, verify_enclave_attestation},
    TransportError,
};
use alloy_primitives::{B256, U256};
use std::sync::{Arc, Mutex};

const TEST_DOMAIN_TAG: &[u8] = b"outbe/tee/test-payload-read/v1";

fn response(amount: u64) -> EnclaveResponse {
    EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash: B256::ZERO,
        outcome: U256::from(amount).to_be_bytes::<32>().to_vec(),
        attestation_tag: vec![0; 64],
    }
}

fn state(mode: WitnessExecutionMode) -> Arc<Mutex<WitnessState>> {
    Arc::new(Mutex::new(WitnessState::new(
        mode,
        Some([1; 32]),
        EnclaveWitnessTapeV1::default(),
    )))
}

fn authenticate(_: &[u8; 32], _: &EnclaveResponse) -> Result<(), TransportError> {
    Ok(())
}

#[test]
fn record_stages_and_replay_never_calls_transport() {
    let s = state(WitnessExecutionMode::ProposerRecord);
    let _scope = WitnessExecutionScope::enter(s.clone());
    consensus_call(
        "ReadPayloadAmount",
        || Ok(([1; 32], response(7))),
        authenticate,
    )
    .unwrap();
    assert_eq!(s.lock().unwrap().staged.len(), 1);
    drop(_scope);
    let replay = state(WitnessExecutionMode::VerifierReplay);
    replay
        .lock()
        .unwrap()
        .tape
        .prefix
        .push(EnclaveWitnessEntryV1::try_from(response(7)).unwrap());
    let _scope = WitnessExecutionScope::enter(replay.clone());
    let (key, resp) = consensus_call(
        "ReadPayloadAmount",
        || panic!("replay opened transport"),
        authenticate,
    )
    .unwrap();
    assert_eq!(key, [1; 32]);
    assert!(matches!(
        resp,
        EnclaveResponse::DcapVerificationFinishedV1 { ref outcome, .. }
            if outcome.as_slice() == U256::from(7).to_be_bytes::<32>()
    ));
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
    assert_eq!(replay.lock().unwrap().first_witness_fault, None);
}

#[test]
fn with_enclave_compares_business_values_and_latches_first_fault() {
    let s = state(WitnessExecutionMode::VerifierWithEnclave);
    s.lock()
        .unwrap()
        .tape
        .prefix
        .push(EnclaveWitnessEntryV1::try_from(response(7)).unwrap());
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Ok(([2; 32], response(8))),
        authenticate
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::SemanticMismatch)
    );
    assert!(consensus_call(
        "ReadPayloadAmount",
        || panic!("sticky fault retried transport"),
        authenticate
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::SemanticMismatch)
    );
}

#[test]
fn unsigned_user_candidate_is_not_a_block_fault_but_prefix_is() {
    let s = state(WitnessExecutionMode::ProposerRecord);
    s.lock().unwrap().tx_index = Some(3);
    s.lock().unwrap().is_user_candidate = true;
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Err(TransportError::EnclaveError("refused".into())),
        authenticate
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().candidate_rejection,
        Some(CandidateRejection::UnsignedEnclaveError)
    );
    assert_eq!(s.lock().unwrap().first_witness_fault, None);
    assert!(s.lock().unwrap().staged.is_empty());
    drop(_scope);
    let prefix = state(WitnessExecutionMode::ProposerRecord);
    let _scope = WitnessExecutionScope::enter(prefix.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Err(TransportError::EnclaveError("refused".into())),
        authenticate
    )
    .is_err());
    assert_eq!(
        prefix.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

fn signed_response(signer: &ed25519_dalek::SigningKey, amount: u64) -> EnclaveResponse {
    use ed25519_dalek::Signer;
    let outcome = U256::from(amount).to_be_bytes::<32>().to_vec();
    let preimage = canonical_attestation_preimage(TEST_DOMAIN_TAG, B256::ZERO, &outcome).unwrap();
    EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash: B256::ZERO,
        outcome,
        attestation_tag: signer.sign(&preimage).to_bytes().to_vec(),
    }
}

fn verify_signed(key: &[u8; 32], response: &EnclaveResponse) -> Result<(), TransportError> {
    let EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash,
        outcome,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if *request_hash != B256::ZERO {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = canonical_attestation_preimage(TEST_DOMAIN_TAG, B256::ZERO, outcome)
        .map_err(TransportError::AttestationPreimageSerialization)?;
    verify_enclave_attestation(key, &preimage, attestation_tag).map_err(TransportError::attestation)
}

#[test]
fn with_enclave_authenticates_both_keys_and_returns_exact_leader_signature() {
    let leader = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let local = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let leader_response = signed_response(&leader, 7);
    let s = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader.verifying_key().to_bytes()),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::try_from(leader_response.clone()).unwrap()],
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(s.clone());
    let (key, response) = consensus_call(
        "ReadPayloadAmount",
        || Ok((local.verifying_key().to_bytes(), signed_response(&local, 7))),
        verify_signed,
    )
    .unwrap();
    assert_eq!(key, leader.verifying_key().to_bytes());
    assert_eq!(response, leader_response);
    assert_eq!(s.lock().unwrap().prefix_cursor, 1);
    assert_eq!(s.lock().unwrap().first_witness_fault, None);
}

#[test]
fn bad_leader_signature_latches_before_transport_and_does_not_consume() {
    let signer = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let s = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(signer.verifying_key().to_bytes()),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::try_from(response(7)).unwrap()],
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || panic!("bad leader tag reached transport"),
        verify_signed
    )
    .is_err());
    let state = s.lock().unwrap();
    assert_eq!(state.prefix_cursor, 0);
    assert_eq!(
        state.first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

#[test]
fn missing_scope_and_keyless_rpc_fail_closed_when_policy_is_installed() {
    set_witness_policy_for_test(Some(WitnessPolicy::ReplayOnly));
    assert!(consensus_call(
        "ReadPayloadAmount",
        || panic!("unscoped prewarm reached transport"),
        authenticate
    )
    .is_err());
    let s = state(WitnessExecutionMode::RpcDirect);
    let _scope = WitnessExecutionScope::enter(s);
    assert!(consensus_call(
        "ReadPayloadAmount",
        || panic!("keyless RPC reached transport"),
        authenticate
    )
    .is_err());
    set_witness_policy_for_test(None);
}

#[test]
fn unsigned_system_body_and_local_transport_outage_are_sticky() {
    let s = state(WitnessExecutionMode::ProposerRecord);
    s.lock().unwrap().tx_index = Some(2);
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Err(TransportError::EnclaveError("refused".into())),
        authenticate
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
    drop(_scope);
    let s = state(WitnessExecutionMode::ProposerRecord);
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Err(TransportError::Unavailable("offline".into())),
        authenticate
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable)
    );
    drop(_scope);
    let leader = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let s = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader.verifying_key().to_bytes()),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::try_from(signed_response(&leader, 7)).unwrap()],
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(consensus_call(
        "ReadPayloadAmount",
        || Ok((
            [9; 32],
            EnclaveResponse::Error {
                message: "local enclave busy".into()
            }
        )),
        verify_signed
    )
    .is_err());
    assert_eq!(
        s.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable)
    );
}

#[test]
fn proposer_record_transport_panic_latches_local_unavailable_without_staging() {
    let s = state(WitnessExecutionMode::ProposerRecord);
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(matches!(
        consensus_call(
            "ReadPayloadAmount",
            || panic!("enclave socket panicked"),
            authenticate
        ),
        Err(TransportError::Unavailable(message)) if message == "local enclave transport panicked"
    ));
    {
        let state = s.lock().unwrap();
        assert_eq!(
            state.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );
        assert!(state.staged.is_empty());
    }
    let transport_called = std::cell::Cell::new(false);
    let latched = consensus_call(
        "ReadPayloadAmount",
        || {
            transport_called.set(true);
            Ok(([1; 32], response(7)))
        },
        authenticate,
    )
    .unwrap_err();
    assert!(latched.is_node_local());
    assert_eq!(latched.metric_class(), "unavailable");
    assert!(matches!(
        std::error::Error::source(&latched)
            .and_then(|cause| cause.downcast_ref::<crate::errors::WitnessAvailabilityError>()),
        Some(crate::errors::WitnessAvailabilityError::LocalUnavailable)
    ));
    assert!(!transport_called.get());
    assert!(s.lock().unwrap().staged.is_empty());
}

#[test]
fn verifier_with_enclave_transport_panic_latches_local_unavailable_without_consuming_entry() {
    let s = state(WitnessExecutionMode::VerifierWithEnclave);
    s.lock()
        .unwrap()
        .tape
        .prefix
        .push(EnclaveWitnessEntryV1::try_from(response(7)).unwrap());
    let _scope = WitnessExecutionScope::enter(s.clone());
    assert!(matches!(
        consensus_call(
            "ReadPayloadAmount",
            || panic!("local enclave crashed"),
            authenticate
        ),
        Err(TransportError::Unavailable(message)) if message == "local enclave transport panicked"
    ));
    let state = s.lock().unwrap();
    assert_eq!(
        state.first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable)
    );
    assert_eq!(state.prefix_cursor, 0);
}

#[test]
fn repeat_after_candidate_rejection_keeps_legacy_enclave_error_and_typed_cause() {
    let legacy = state(WitnessExecutionMode::ProposerRecord);
    legacy.lock().unwrap().candidate_rejection = Some(CandidateRejection::UnsignedEnclaveError);
    let _scope = WitnessExecutionScope::enter(legacy);
    let legacy_error = consensus_call(
        "ReadPayloadAmount",
        || panic!("rejected candidate retried transport"),
        authenticate,
    )
    .unwrap_err();
    assert!(matches!(legacy_error, TransportError::EnclaveError(_)));
    assert_eq!(
        legacy_error.to_string(),
        "enclave returned error: witness candidate already rejected"
    );
    drop(_scope);

    let typed = state(WitnessExecutionMode::ProposerRecord);
    typed.lock().unwrap().candidate_rejection = Some(CandidateRejection::UnsignedEnclaveError);
    let _scope = WitnessExecutionScope::enter(typed);
    let typed_error = crate::tee_consensus_typed_call(
        crate::CallMeta::recorded("candidate_reject"),
        |_: &crate::in_process::InProcessTeeContext| -> Result<(), TransportError> {
            panic!("rejected candidate retried transport")
        },
        |_: &()| panic!("rejected candidate encoded"),
        |_, _| panic!("rejected candidate decoded"),
    )
    .unwrap_err();
    assert!(matches!(typed_error, TransportError::EnclaveCause { .. }));
    assert!(std::error::Error::source(&typed_error)
        .is_some_and(|cause| { cause.is::<crate::errors::WitnessCandidateRejected>() }));
}
