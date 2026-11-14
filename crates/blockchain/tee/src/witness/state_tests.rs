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
    EnclaveResponse::Payload {
        recordable: true,
        payload: U256::from(amount).to_be_bytes::<32>().to_vec(),
        inputs_canonical_hash: B256::ZERO,
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
        EnclaveResponse::Payload { ref payload, .. }
            if payload.as_slice() == U256::from(7).to_be_bytes::<32>()
    ));
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
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
    let payload = U256::from(amount).to_be_bytes::<32>().to_vec();
    let preimage = canonical_attestation_preimage(TEST_DOMAIN_TAG, B256::ZERO, &payload).unwrap();
    EnclaveResponse::Payload {
        recordable: true,
        payload,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: signer.sign(&preimage).to_bytes().to_vec(),
    }
}

fn verify_signed(key: &[u8; 32], response: &EnclaveResponse) -> Result<(), TransportError> {
    let EnclaveResponse::Payload {
        recordable: true,
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if *inputs_canonical_hash != B256::ZERO {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = canonical_attestation_preimage(TEST_DOMAIN_TAG, B256::ZERO, payload)
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
