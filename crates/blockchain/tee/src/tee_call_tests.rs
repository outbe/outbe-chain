use ed25519_dalek::{Signer as _, SigningKey};

use super::*;

#[test]
fn signed_rejection_and_lazy_attestation_enforce_domain_hash_and_tag_len() {
    let signer = SigningKey::from_bytes(&[0x5a; 32]);
    let pubkey = signer.verifying_key().to_bytes();
    let hash = canonical_domain_hash(b"outbe/test/input/v1", &"payload").unwrap();
    let reason = "rejected-for-test";
    let preimage = canonical_attestation_preimage(b"outbe/test/reject/v1", hash, reason).unwrap();
    let tag = signer.sign(&preimage).to_bytes();

    verify_signed_rejection(
        &pubkey,
        SignedRejectionRef {
            domain: b"outbe/test/reject/v1",
            expected_inputs_hash: hash,
            actual_inputs_hash: hash,
            reason,
            tag: &tag,
        },
    )
    .expect("matching domain and hash verify");

    assert!(matches!(
        verify_signed_rejection(
            &pubkey,
            SignedRejectionRef {
                domain: b"outbe/test/other/v1",
                expected_inputs_hash: hash,
                actual_inputs_hash: hash,
                reason,
                tag: &tag,
            },
        ),
        Err(TeeAttestationError::SignatureMismatch(_))
    ));

    let other_hash = B256::repeat_byte(0x99);
    assert!(matches!(
        verify_signed_rejection(
            &pubkey,
            SignedRejectionRef {
                domain: b"outbe/test/reject/v1",
                expected_inputs_hash: hash,
                actual_inputs_hash: other_hash,
                reason,
                tag: &tag,
            },
        ),
        Err(TeeAttestationError::InputsHashMismatch { .. })
    ));

    assert!(matches!(
        verify_enclave_attestation(&pubkey, &preimage, &tag[..63]),
        Err(TeeAttestationError::InvalidSignatureLength { actual: 63 })
    ));
}

fn binding() -> outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    NetworkBindingV1 {
        chain_id: alloy_primitives::U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    }
}

fn install_context() -> (crate::in_process_dispatch::InProcessTestGuard, [u8; 32]) {
    let context = std::sync::Arc::new(crate::test_utils::resident_context(4, binding()));
    let leader = context.attestation_public();
    let guard = crate::in_process_dispatch::set_in_process_tee_context_for_test(Some(context));
    (guard, leader)
}

fn decode_u64(
    key: &[u8; 32],
    expected: &[u8; 32],
    parts: crate::witness::TypedWitnessPayloadRef<'_>,
) -> Result<u64, crate::TransportError> {
    if key != expected || parts.payload != 11u64.to_be_bytes() {
        return Err(crate::TransportError::UnexpectedResponse);
    }
    Ok(11)
}

fn plain_payload(
    payload: Vec<u8>,
) -> Result<crate::witness::TypedWitnessPayload, crate::TransportError> {
    Ok(crate::witness::TypedWitnessPayload {
        payload,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    })
}

fn decode_live_u64(
    key: &[u8; 32],
    expected: &[u8; 32],
    parts: crate::witness::TypedWitnessPayloadRef<'_>,
) -> Result<u64, crate::TransportError> {
    if key != expected {
        return Err(crate::TransportError::UnexpectedResponse);
    }
    let bytes: [u8; 8] = parts
        .payload
        .try_into()
        .map_err(|_| crate::TransportError::UnexpectedResponse)?;
    Ok(u64::from_be_bytes(bytes))
}

#[test]
fn tee_consensus_typed_call_records_payload_and_replays_without_execution() {
    use crate::witness::{
        EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let calls = std::cell::Cell::new(0u32);
    let value = tee_consensus_typed_call(
        CallMeta::recorded("typed_read"),
        |_| {
            calls.set(calls.get() + 1);
            Ok(11u64)
        },
        |value| plain_payload(value.to_be_bytes().to_vec()),
        |key, parts| decode_u64(key, &leader, parts),
    )
    .unwrap();
    assert_eq!(value, 11);
    assert_eq!(calls.get(), 1);
    let staged = state.lock().unwrap().staged.clone();
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    assert_eq!(payload.as_slice(), 11u64.to_be_bytes());
    assert_eq!(*inputs_canonical_hash, B256::ZERO);
    assert!(attestation_tag.is_empty());
    for (index, entry) in [
        EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: Vec::new(),
            attestation_tag: Vec::new(),
        },
        EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: Vec::new(),
            onboarding_artifact: Vec::new(),
            attestation_tag: Vec::new(),
        },
        EnclaveWitnessEntryV1::NoAttestationDevOnboardingArtifactPreparedV2 {
            request_hash: B256::ZERO,
            onboarding_artifact: Vec::new(),
            attestation_tag: Vec::new(),
        },
        staged[0].clone(),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(postcard::to_allocvec(&entry).unwrap()[0], index as u8);
    }
    drop(_scope);

    let replay = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged.clone(),
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(replay.clone());
    let replayed = tee_consensus_typed_call(
        CallMeta::recorded("typed_read"),
        |_| panic!("replay executed the closure"),
        |_| panic!("replay encoded a live value"),
        |key, parts| decode_u64(key, &leader, parts),
    )
    .unwrap();
    assert_eq!(replayed, 11);
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
    drop(_scope);

    let mut tampered = staged;
    let EnclaveWitnessEntryV1::Payload { payload, .. } = &mut tampered[0] else {
        panic!("recorded entry is not Payload");
    };
    payload[0] ^= 0xff;
    let bad = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: tampered,
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(bad.clone());
    assert!(tee_consensus_typed_call(
        CallMeta::recorded("typed_read"),
        |_| panic!("tampered replay executed"),
        |_| panic!("tampered replay encoded"),
        |key, parts| decode_u64(key, &leader, parts),
    )
    .is_err());
    assert_eq!(
        bad.lock().unwrap().first_witness_fault,
        Some(crate::witness::BlockWitnessFault::TapeInvalid)
    );
    assert_eq!(bad.lock().unwrap().prefix_cursor, 0);
}

#[test]
fn typed_calls_fail_closed_on_the_recordable_flag() {
    let consensus = tee_consensus_typed_call(
        CallMeta::session_only("session"),
        |_| panic!("non-recordable consensus call executed"),
        |_: &u8| panic!("encoded"),
        |_, _| panic!("decoded"),
    )
    .unwrap_err();
    assert!(matches!(
        consensus,
        crate::TransportError::CodecCause { .. }
    ));
    assert_eq!(consensus.metric_class(), "codec");
    assert!(!consensus.is_node_local());
    assert!(matches!(
        std::error::Error::source(&consensus)
            .and_then(|cause| cause.downcast_ref::<crate::errors::TypedCallRouteError>()),
        Some(crate::errors::TypedCallRouteError::NonRecordableConsensus)
    ));

    let session = tee_session_typed_call(
        CallMeta::recorded("recorded"),
        |_| -> Result<(), crate::TransportError> { panic!("recordable session call executed") },
    )
    .expect("recordable mismatch is an error, not a missing runtime")
    .unwrap_err();
    assert!(matches!(session, crate::TransportError::CodecCause { .. }));
    assert_eq!(session.metric_class(), "codec");
    assert!(!session.is_node_local());
    assert!(matches!(
        std::error::Error::source(&session)
            .and_then(|cause| cause.downcast_ref::<crate::errors::TypedCallRouteError>()),
        Some(crate::errors::TypedCallRouteError::RecordableSession)
    ));
    assert!(tee_session_typed_call(CallMeta::session_only("missing"), |_| Ok(())).is_none());
}

#[test]
fn tee_consensus_typed_call_rpc_direct_executes_without_encoding() {
    use crate::witness::{
        EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::RpcDirect,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let (executed, encoded) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
    let value = tee_consensus_typed_call(
        CallMeta::recorded("rpc_direct"),
        |_| {
            executed.set(executed.get() + 1);
            Ok(11u64)
        },
        |_| {
            encoded.set(encoded.get() + 1);
            plain_payload(Vec::new())
        },
        |_, _| panic!("rpc direct authenticated a witness entry"),
    )
    .unwrap();
    assert_eq!(value, 11);
    assert_eq!(executed.get(), 1);
    assert_eq!(encoded.get(), 0);
    assert!(state.lock().unwrap().staged.is_empty());
    assert_eq!(state.lock().unwrap().prefix_cursor, 0);
    drop(_scope);

    let (executed, encoded) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
    let value = tee_consensus_typed_call(
        CallMeta::recorded("unscoped"),
        |_| {
            executed.set(executed.get() + 1);
            Ok(4u64)
        },
        |_| {
            encoded.set(encoded.get() + 1);
            plain_payload(Vec::new())
        },
        |_, _| panic!("unscoped call authenticated a witness entry"),
    )
    .unwrap();
    assert_eq!(value, 4);
    assert_eq!(executed.get(), 1);
    assert_eq!(encoded.get(), 0);
}

#[test]
fn tee_consensus_typed_call_with_enclave_compares_the_live_encoding() {
    use crate::witness::{
        EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let recorded = 11u64.to_be_bytes().to_vec();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::Payload {
                payload: recorded,
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: Vec::new(),
            }],
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let (executed, encoded) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
    let value = tee_consensus_typed_call(
        CallMeta::recorded("with_enclave"),
        |_| {
            executed.set(executed.get() + 1);
            Ok(11u64)
        },
        |value| {
            encoded.set(encoded.get() + 1);
            plain_payload(value.to_be_bytes().to_vec())
        },
        |key, parts| decode_u64(key, &leader, parts),
    )
    .unwrap();
    assert_eq!(value, 11);
    assert_eq!(executed.get(), 1);
    assert_eq!(encoded.get(), 1);
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    assert_eq!(state.lock().unwrap().first_witness_fault, None);
}

#[test]
fn tee_consensus_typed_call_with_enclave_rejects_divergent_live_encoding() {
    use crate::witness::{
        BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
        WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: vec![EnclaveWitnessEntryV1::Payload {
                payload: 11u64.to_be_bytes().to_vec(),
                inputs_canonical_hash: B256::ZERO,
                attestation_tag: Vec::new(),
            }],
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let (executed, encoded) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
    let error = tee_consensus_typed_call(
        CallMeta::recorded("with_enclave_mismatch"),
        |_| {
            executed.set(executed.get() + 1);
            Ok(11u64)
        },
        |value| {
            encoded.set(encoded.get() + 1);
            assert_eq!(*value, 11);
            plain_payload(12u64.to_be_bytes().to_vec())
        },
        |key, parts| decode_live_u64(key, &leader, parts),
    )
    .unwrap_err();
    assert!(error.is_node_local());
    assert_eq!(error.metric_class(), "unavailable");
    assert!(matches!(
        std::error::Error::source(&error)
            .and_then(|cause| cause.downcast_ref::<crate::errors::WitnessAvailabilityError>()),
        Some(crate::errors::WitnessAvailabilityError::SemanticMismatch)
    ));
    assert_eq!(executed.get(), 1);
    assert_eq!(encoded.get(), 1);
    let witness = state.lock().unwrap();
    assert_eq!(witness.prefix_cursor, 0);
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::SemanticMismatch)
    );
}

fn assert_witness_unavailable(
    error: &crate::TransportError,
    expected: crate::errors::WitnessAvailabilityError,
) {
    assert!(error.is_node_local());
    assert_eq!(error.metric_class(), "unavailable");
    assert!(matches!(
        std::error::Error::source(error)
            .and_then(|cause| cause.downcast_ref::<crate::errors::WitnessAvailabilityError>()),
        Some(cause) if std::mem::discriminant(cause) == std::mem::discriminant(&expected)
    ));
}

#[test]
fn tee_consensus_typed_call_empty_tape_and_latched_fault_keep_typed_sources() {
    use crate::witness::{
        BlockWitnessFault, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let executed = std::cell::Cell::new(0u32);
    let call = || {
        tee_consensus_typed_call(
            CallMeta::recorded("empty_tape"),
            |_| {
                executed.set(executed.get() + 1);
                Ok(())
            },
            |_: &()| panic!("empty tape encoded"),
            |_, _| panic!("empty tape decoded"),
        )
    };
    let error = call().unwrap_err();
    assert_witness_unavailable(&error, crate::errors::WitnessAvailabilityError::TapeInvalid);
    assert_eq!(executed.get(), 0);
    assert_eq!(
        state.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
    assert_eq!(state.lock().unwrap().prefix_cursor, 0);
    let again = call().unwrap_err();
    assert_witness_unavailable(&again, crate::errors::WitnessAvailabilityError::TapeInvalid);
    assert_eq!(executed.get(), 0);
    assert_eq!(state.lock().unwrap().prefix_cursor, 0);
}

#[test]
fn tee_consensus_typed_call_proposer_key_mismatch_keeps_typed_source() {
    use crate::witness::{
        EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let mut other = leader;
    other[0] ^= 1;
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(other),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let executed = std::cell::Cell::new(0u32);
    let call = || {
        tee_consensus_typed_call(
            CallMeta::recorded("proposer_key"),
            |_| {
                executed.set(executed.get() + 1);
                Ok(7u64)
            },
            |_| panic!("mismatched proposer key encoded"),
            |_, _| panic!("mismatched proposer key decoded"),
        )
    };
    let error = call().unwrap_err();
    assert_witness_unavailable(
        &error,
        crate::errors::WitnessAvailabilityError::LocalUnavailable,
    );
    assert_eq!(executed.get(), 1);
    assert!(state.lock().unwrap().staged.is_empty());
    let again = call().unwrap_err();
    assert_witness_unavailable(
        &again,
        crate::errors::WitnessAvailabilityError::LocalUnavailable,
    );
    assert_eq!(executed.get(), 1);
    assert!(state.lock().unwrap().staged.is_empty());
}

#[test]
fn tee_consensus_typed_call_repeat_after_candidate_rejection_keeps_typed_source() {
    use crate::witness::{
        CandidateRejection, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    state.lock().unwrap().tx_index = Some(3);
    state.lock().unwrap().is_user_candidate = true;
    let _scope = WitnessExecutionScope::enter(state.clone());
    let executed = std::cell::Cell::new(0u32);
    let call = || {
        tee_consensus_typed_call(
            CallMeta::recorded("candidate_reject"),
            |_| {
                executed.set(executed.get() + 1);
                Err(crate::TransportError::enclave_error(std::io::Error::other(
                    "refused",
                )))
            },
            |_: &()| panic!("rejected candidate encoded"),
            |_, _| panic!("rejected candidate decoded"),
        )
    };
    let first = call().unwrap_err();
    assert!(matches!(first, crate::TransportError::EnclaveCause { .. }));
    assert_eq!(executed.get(), 1);
    assert_eq!(
        state.lock().unwrap().candidate_rejection,
        Some(CandidateRejection::UnsignedEnclaveError)
    );
    assert!(state.lock().unwrap().first_witness_fault.is_none());
    assert!(state.lock().unwrap().staged.is_empty());
    let again = call().unwrap_err();
    assert!(!again.is_node_local());
    assert_eq!(again.metric_class(), "enclave_error");
    assert!(std::error::Error::source(&again)
        .is_some_and(|cause| { cause.is::<crate::errors::WitnessCandidateRejected>() }));
    assert_eq!(executed.get(), 1);
    assert!(state.lock().unwrap().staged.is_empty());
}

#[test]
fn tee_session_typed_call_skips_serialization_and_witness_mutation() {
    use crate::witness::{
        EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    let (_guard, leader) = install_context();
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let recorder = SessionMetrics {
        errors: Arc::new(AtomicU64::new(0)),
        durations: Arc::new(AtomicU64::new(0)),
        names: std::cell::RefCell::new(Vec::new()),
    };
    metrics::with_local_recorder(&recorder, || {
        let value = tee_session_typed_call(CallMeta::session_only("account_query"), |_| Ok(7u8))
            .expect("context installed")
            .unwrap();
        assert_eq!(value, 7);
        let error = tee_session_typed_call(CallMeta::session_only("canary"), |_| {
            Err::<(), _>(crate::TransportError::unavailable(std::io::Error::other(
                "down",
            )))
        })
        .unwrap()
        .unwrap_err();
        assert!(error.is_node_local());
    });
    assert_eq!(recorder.durations.load(Ordering::Relaxed), 2);
    assert_eq!(recorder.errors.load(Ordering::Relaxed), 1);
    let names = recorder.names.borrow().clone();
    assert!(names
        .iter()
        .any(|name| name == "outbe_tee_client_request_duration_ms"));
    assert!(names
        .iter()
        .any(|name| name == "outbe_tee_client_request_errors_total"));
    assert!(state.lock().unwrap().staged.is_empty());
    assert_eq!(state.lock().unwrap().prefix_cursor, 0);
}

struct SessionMetrics {
    errors: std::sync::Arc<std::sync::atomic::AtomicU64>,
    durations: std::sync::Arc<std::sync::atomic::AtomicU64>,
    names: std::cell::RefCell<Vec<String>>,
}

struct DurationHistogram(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl metrics::HistogramFn for DurationHistogram {
    fn record(&self, _value: f64) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl metrics::Recorder for SessionMetrics {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        self.names.borrow_mut().push(key.name().to_owned());
        metrics::Counter::from_arc(std::sync::Arc::clone(&self.errors))
    }
    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        key: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        self.names.borrow_mut().push(key.name().to_owned());
        metrics::Histogram::from_arc(std::sync::Arc::new(DurationHistogram(
            std::sync::Arc::clone(&self.durations),
        )))
    }
}
