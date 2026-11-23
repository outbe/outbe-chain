use std::error::Error as _;

use super::*;

/// Independent `(variant, discriminant)` table. Swapping two enum
/// discriminants fails this list even though `value()` is `as u8`.
const RECORDABLE_V1_TAG_LITERALS: [(WitnessResponseTag, u8); 20] = [
    (WitnessResponseTag::GratisOpApplied, 1),
    (WitnessResponseTag::PromisOpApplied, 2),
    (WitnessResponseTag::FidelityLeaguesSnapshotted, 4),
    (WitnessResponseTag::FidelityIndexQueried, 5),
    (WitnessResponseTag::NodEncryptedForTributeV1, 7),
    (WitnessResponseTag::EncryptedNodMintRejectedV1, 8),
    (WitnessResponseTag::EncryptedNodMinedV1, 9),
    (WitnessResponseTag::NodAmountReadV1, 10),
    (WitnessResponseTag::EncryptedTributeOfferBatchV2, 11),
    (WitnessResponseTag::TributeAmountsReadV2, 12),
    (WitnessResponseTag::TributeDayOpAppliedV2, 13),
    (WitnessResponseTag::TributeDayAmountReadV2, 14),
    (WitnessResponseTag::FidelityQueryRejected, 15),
    (WitnessResponseTag::LysisFractionsExecutedV1, 16),
    (WitnessResponseTag::TributeDayFractionsReadV1, 17),
    (WitnessResponseTag::NodIssuedFromTributeV1, 18),
    (WitnessResponseTag::NodGroupLoadOpAppliedV1, 19),
    (WitnessResponseTag::NodGroupLoadReadV1, 20),
    (WitnessResponseTag::NodQuoteEncryptedV1, 21),
    (WitnessResponseTag::NodIssuedAtOfferV1, 22),
];

#[test]
fn recordable_tags_round_trip_and_reject_non_recordable_bytes() {
    assert_eq!(
        RECORDABLE_V1_TAG_LITERALS.len(),
        WitnessResponseTag::ALL.len()
    );
    for (index, (tag, literal)) in RECORDABLE_V1_TAG_LITERALS.iter().copied().enumerate() {
        assert_eq!(WitnessResponseTag::ALL[index], tag);
        assert_eq!(tag.value(), literal);
        let payload = encode_tagged_payload(tag, &literal).unwrap();
        assert_eq!(payload[0], literal);
        assert_eq!(decode_tagged_payload::<u8>(tag, &payload).unwrap(), literal);
        assert_eq!(WitnessResponseTag::try_from(literal).unwrap(), tag);
    }
    for tag in NON_RECORDABLE_V1_TAGS {
        let error = WitnessResponseTag::try_from(tag).unwrap_err();
        assert_eq!(error, WitnessTagError::NonRecordable { tag });
        let rejected = decode_tagged_payload::<u8>(WitnessResponseTag::GratisOpApplied, &[tag]);
        let error = rejected.unwrap_err();
        assert!(matches!(
            error
                .source()
                .and_then(|cause| cause.downcast_ref::<WitnessTagError>()),
            Some(WitnessTagError::NonRecordable { .. })
        ));
    }
    assert!(matches!(
        WitnessResponseTag::try_from(23),
        Err(WitnessTagError::OutOfRange { tag: 23 })
    ));
    assert_eq!(
        (0..=V1_RESPONSE_TAG_MAX)
            .filter(|tag| WitnessResponseTag::try_from(*tag).is_ok())
            .count(),
        WitnessResponseTag::ALL.len()
    );
}

#[test]
fn decode_rejects_trailing_bytes_and_noncanonical_bool_or_integer() {
    let tag = WitnessResponseTag::GratisOpApplied;
    let mut trailing = encode_tagged_payload(tag, &1u16).unwrap();
    trailing.push(0);
    assert_eq!(
        tag_error(decode_tagged_payload::<u16>(tag, &trailing)),
        WitnessTagError::TrailingBytes
    );

    let bad_bool = [1, 2];
    let error = decode_tagged_payload::<bool>(tag, &bad_bool).unwrap_err();
    assert!(error
        .source()
        .and_then(|cause| cause.downcast_ref::<postcard::Error>())
        .is_some());

    let overlong = [1, 0x81, 0x00];
    assert_eq!(
        tag_error(decode_tagged_payload::<u16>(tag, &overlong)),
        WitnessTagError::NonCanonical
    );
}

fn tag_error<T: std::fmt::Debug>(result: Result<T, TransportError>) -> WitnessTagError {
    let error = result.expect_err("payload must be rejected");
    *error
        .source()
        .and_then(|cause| cause.downcast_ref::<WitnessTagError>())
        .expect("typed witness tag cause")
}

#[test]
fn typed_calls_keep_the_recordable_route_source() {
    let consensus = consensus_witness_call(
        CallMeta::session_only("session"),
        |_| -> Result<SignedBody, TransportError> {
            panic!("non-recordable consensus call executed")
        },
        |_, _| panic!("decoded"),
    )
    .unwrap_err();
    assert!(consensus.source().is_some());

    let session = session_witness_call(
        CallMeta::recorded("recorded"),
        |_| -> Result<(), TransportError> { panic!("recordable session call executed") },
    )
    .expect("recordable mismatch is an error")
    .unwrap_err();
    assert!(session.source().is_some());
    assert!(session_witness_call(CallMeta::session_only("missing"), |_| Ok(())).is_none());
}

const WITNESS_DOMAIN: &[u8] = b"outbe/witness/envelope/v1";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SignedBody {
    amount: u64,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

impl WitnessSchema for SignedBody {
    const TAG: WitnessResponseTag = WitnessResponseTag::GratisOpApplied;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct CanonProbe {
    flag: bool,
    n: u16,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

impl WitnessSchema for CanonProbe {
    const TAG: WitnessResponseTag = WitnessResponseTag::GratisOpApplied;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}

fn witness_binding() -> outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    NetworkBindingV1 {
        chain_id: alloy_primitives::U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
    }
}

fn witness_context(seed: u8) -> std::sync::Arc<outbe_tee::in_process::InProcessTeeContext> {
    std::sync::Arc::new(outbe_tee::test_utils::resident_context(
        seed,
        witness_binding(),
    ))
}

fn install_context(
    context: std::sync::Arc<outbe_tee::in_process::InProcessTeeContext>,
) -> outbe_tee::in_process_dispatch::InProcessTestGuard {
    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(context))
}

fn signed_body(amount: u64, inputs: B256) -> SignedBody {
    SignedBody {
        amount,
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    }
}

fn sign_body(
    context: &outbe_tee::in_process::InProcessTeeContext,
    mut body: SignedBody,
) -> Result<SignedBody, TransportError> {
    body.attestation_tag.clear();
    let preimage =
        super::super::response_auth::attestation(WITNESS_DOMAIN, body.inputs_canonical_hash, &body)
            .map_err(TransportError::codec)?;
    body.attestation_tag =
        super::super::response_auth::sign_attestation_tag(context, &preimage).to_vec();
    Ok(body)
}

fn authenticate_body(key: &[u8; 32], value: &SignedBody) -> Result<(), TransportError> {
    let mut unsigned = value.clone();
    unsigned.attestation_tag.clear();
    let preimage = super::super::response_auth::attestation(
        WITNESS_DOMAIN,
        value.inputs_canonical_hash,
        &unsigned,
    )
    .map_err(TransportError::codec)?;
    super::super::response_auth::verify_response_attestation(
        &super::super::response_auth::ResponseAuthSpec {
            attestation_pub: key,
            descriptor: super::super::GRATIS_OP_ATTESTATION,
            expected_inputs_hash: value.inputs_canonical_hash,
            actual_inputs_hash: value.inputs_canonical_hash,
            tag: &value.attestation_tag,
        },
        &preimage,
    )
}

fn record(
    seed: u8,
    value: SignedBody,
) -> (Vec<outbe_tee::witness::EnclaveWitnessEntryV1>, [u8; 32]) {
    use outbe_tee::witness::{
        EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let context = witness_context(seed);
    let key = context.attestation_public();
    let _guard = install_context(context);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(key),
        EnclaveWitnessTapeV1::default(),
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let recorded = consensus_witness_call(
        CallMeta::recorded("signed_witness"),
        {
            let value = value.clone();
            move |context| sign_body(context, value)
        },
        authenticate_body,
    )
    .unwrap();
    assert_eq!(recorded.amount, value.amount);
    assert_eq!(recorded.inputs_canonical_hash, value.inputs_canonical_hash);
    let staged = state.lock().unwrap().staged.clone();
    (staged, key)
}

#[test]
fn proposer_replay_matches_tag_free_postcard_and_rejects_one_bit() {
    use outbe_tee::witness::{
        BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
        WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let inputs = B256::repeat_byte(0x5a);
    let body = signed_body(11, inputs);
    let (staged, leader) = record(4, body.clone());
    let leader_ctx = witness_context(4);
    let preimage = super::super::response_auth::attestation(WITNESS_DOMAIN, inputs, &body).unwrap();
    let leader_sig = super::super::response_auth::sign_attestation_tag(&leader_ctx, &preimage);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    let mut expected = vec![WitnessResponseTag::GratisOpApplied.value()];
    expected.extend(postcard::to_allocvec(&body).unwrap());
    assert_eq!(payload, &expected);
    assert_eq!(*inputs_canonical_hash, inputs);
    assert_eq!(attestation_tag.as_slice(), leader_sig.as_slice());

    let replay = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged.clone(),
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(replay.clone());
    let value = consensus_witness_call(
        CallMeta::recorded("signed_witness"),
        |_| -> Result<SignedBody, TransportError> { panic!("replay executed") },
        authenticate_body,
    )
    .unwrap();
    assert_eq!(value.amount, body.amount);
    assert_eq!(value.attestation_tag.as_slice(), leader_sig.as_slice());
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
    drop(_scope);

    for corrupt_tag in [true, false] {
        let mut bad = staged.clone();
        let EnclaveWitnessEntryV1::Payload {
            attestation_tag,
            inputs_canonical_hash,
            ..
        } = &mut bad[0]
        else {
            panic!("recorded entry is not Payload");
        };
        if corrupt_tag {
            attestation_tag[0] ^= 0x01;
        } else {
            let mut bytes = inputs_canonical_hash.0;
            bytes[0] ^= 0x01;
            *inputs_canonical_hash = B256::from(bytes);
        }
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some(leader),
            EnclaveWitnessTapeV1 {
                prefix: bad,
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        assert!(consensus_witness_call(
            CallMeta::recorded("signed_witness"),
            |_| -> Result<SignedBody, TransportError> { panic!("corrupt replay executed") },
            authenticate_body,
        )
        .is_err());
        let witness = state.lock().unwrap();
        assert_eq!(witness.prefix_cursor, 0);
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::TapeInvalid)
        );
    }
}

#[test]
fn distinct_keys_accept_different_tags_and_reject_body_or_hash_changes() {
    use outbe_tee::witness::{
        BlockWitnessFault, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let inputs = B256::repeat_byte(0x5a);
    let body = signed_body(11, inputs);
    let (staged, leader) = record(4, body.clone());
    let verifier = witness_context(9);
    assert_ne!(leader, verifier.attestation_public());
    let preimage = super::super::response_auth::attestation(WITNESS_DOMAIN, inputs, &body).unwrap();
    let leader_sig =
        super::super::response_auth::sign_attestation_tag(&witness_context(4), &preimage);
    let verifier_sig = super::super::response_auth::sign_attestation_tag(&verifier, &preimage);
    assert_ne!(leader_sig.as_slice(), verifier_sig.as_slice());

    let _guard = install_context(verifier);
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged.clone(),
            txs: Vec::new(),
        },
    )));
    let _scope = WitnessExecutionScope::enter(state.clone());
    let value = consensus_witness_call(
        CallMeta::recorded("distinct_keys"),
        {
            let body = body.clone();
            move |context| sign_body(context, body)
        },
        authenticate_body,
    )
    .unwrap();
    assert_eq!(value.amount, body.amount);
    assert_eq!(value.attestation_tag.as_slice(), leader_sig.as_slice());
    assert_eq!(state.lock().unwrap().prefix_cursor, 1);
    assert_eq!(state.lock().unwrap().first_witness_fault, None);
    drop(_scope);

    for mut live in [signed_body(12, inputs), {
        let mut changed = body.clone();
        changed.inputs_canonical_hash = B256::repeat_byte(0x5b);
        changed
    }] {
        let _ = &mut live;
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierWithEnclave,
            Some(leader),
            EnclaveWitnessTapeV1 {
                prefix: staged.clone(),
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        assert!(consensus_witness_call(
            CallMeta::recorded("divergent_parts"),
            {
                let live = live.clone();
                move |context| sign_body(context, live)
            },
            authenticate_body,
        )
        .is_err());
        let witness = state.lock().unwrap();
        assert_eq!(witness.prefix_cursor, 0);
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::SemanticMismatch)
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AppliedProbe {
    n: u16,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RejectedProbe {
    reason: String,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

impl WitnessSchema for AppliedProbe {
    const TAG: WitnessResponseTag = WitnessResponseTag::FidelityIndexQueried;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}

impl WitnessSchema for RejectedProbe {
    const TAG: WitnessResponseTag = WitnessResponseTag::FidelityQueryRejected;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}

#[test]
fn dual_answer_encodes_the_selected_schema_without_a_second_discriminant() {
    use outbe_tee::witness::{
        EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let inputs = B256::repeat_byte(0x5c);
    let applied = AppliedProbe {
        n: 7,
        inputs_canonical_hash: inputs,
        attestation_tag: vec![81, 82],
    };
    let rejected = RejectedProbe {
        reason: "rejected".to_string(),
        inputs_canonical_hash: inputs,
        attestation_tag: vec![81, 82],
    };
    let mut cleared_applied = applied.clone();
    cleared_applied.attestation_tag.clear();
    let mut cleared_rejected = rejected.clone();
    cleared_rejected.attestation_tag.clear();
    let cases = [
        (
            DualWitnessAnswer::<AppliedProbe, RejectedProbe>::Applied(applied.clone()),
            WitnessResponseTag::FidelityIndexQueried,
            postcard::to_allocvec(&cleared_applied).unwrap(),
        ),
        (
            DualWitnessAnswer::Rejected(rejected.clone()),
            WitnessResponseTag::FidelityQueryRejected,
            postcard::to_allocvec(&cleared_rejected).unwrap(),
        ),
    ];
    let context = witness_context(4);
    let leader = context.attestation_public();
    let _guard = install_context(context);
    for (value, tag, body) in cases {
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::ProposerRecord,
            Some(leader),
            EnclaveWitnessTapeV1::default(),
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        let recorded = consensus_witness_call(
            CallMeta::recorded("dual_schema"),
            {
                let value = value.clone();
                move |_| Ok(value)
            },
            |_, decoded: &DualWitnessAnswer<AppliedProbe, RejectedProbe>| {
                if decoded.attestation_tag() == [81, 82] {
                    Ok(())
                } else {
                    Err(TransportError::UnexpectedResponse)
                }
            },
        )
        .unwrap();
        assert_eq!(recorded, value);
        let staged = state.lock().unwrap().staged.clone();
        drop(_scope);
        let EnclaveWitnessEntryV1::Payload { payload, .. } = &staged[0] else {
            panic!("recorded entry is not Payload");
        };
        assert_eq!(payload[0], tag.value());
        assert_eq!(&payload[1..], body.as_slice());
        let replay = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some(leader),
            EnclaveWitnessTapeV1 {
                prefix: staged,
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(replay);
        let decoded = consensus_witness_call(
            CallMeta::recorded("dual_schema"),
            |_| -> Result<DualWitnessAnswer<AppliedProbe, RejectedProbe>, TransportError> {
                panic!("replay executed")
            },
            |_, decoded: &DualWitnessAnswer<AppliedProbe, RejectedProbe>| {
                if decoded.attestation_tag() == [81, 82] {
                    Ok(())
                } else {
                    Err(TransportError::UnexpectedResponse)
                }
            },
        )
        .unwrap();
        assert_eq!(decoded, value);
    }
}

#[test]
fn replay_rejects_trailing_noncanonical_inner_tag_and_hash_mismatch() {
    use outbe_tee::witness::{
        BlockWitnessFault, EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode,
        WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let honest = CanonProbe {
        flag: false,
        n: 1,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    };
    let canonical = encode_canonical_body(&honest).unwrap();
    let mut trailing = prefix_tag(WitnessResponseTag::GratisOpApplied, canonical.clone());
    trailing.push(0xff);
    let mut overlong = canonical.clone();
    assert_eq!(overlong[1], 1);
    overlong.splice(1..2, [0x81, 0x00]);
    let overlong = prefix_tag(WitnessResponseTag::GratisOpApplied, overlong);
    let mut bad_bool = canonical.clone();
    bad_bool[0] = 2;
    let bad_bool = prefix_tag(WitnessResponseTag::GratisOpApplied, bad_bool);
    let mut tagged = honest.clone();
    tagged.attestation_tag = vec![9];
    let inner_tag = prefix_tag(
        WitnessResponseTag::GratisOpApplied,
        encode_canonical_body(&tagged).unwrap(),
    );
    let mut other_hash = honest.clone();
    other_hash.inputs_canonical_hash = B256::repeat_byte(0x11);
    let hash_mismatch = prefix_tag(
        WitnessResponseTag::GratisOpApplied,
        encode_canonical_body(&other_hash).unwrap(),
    );
    let cases = [
        (trailing, WitnessTagError::TrailingBytes),
        (overlong, WitnessTagError::NonCanonical),
        (inner_tag, WitnessTagError::InnerAttestationTag),
        (hash_mismatch, WitnessTagError::InputsHashMismatch),
    ];
    let reject = |payload: Vec<u8>| -> TransportError {
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::VerifierReplay,
            Some([4; 32]),
            EnclaveWitnessTapeV1 {
                prefix: vec![EnclaveWitnessEntryV1::Payload {
                    payload,
                    inputs_canonical_hash: B256::ZERO,
                    attestation_tag: vec![81, 82],
                }],
                txs: Vec::new(),
            },
        )));
        let _scope = WitnessExecutionScope::enter(state.clone());
        let error = consensus_witness_call(
            CallMeta::recorded("reject_wire"),
            |_| -> Result<CanonProbe, TransportError> { panic!("replay executed") },
            |_, _: &CanonProbe| panic!("signature check ran before the payload was rejected"),
        )
        .unwrap_err();
        let witness = state.lock().unwrap();
        assert_eq!(witness.prefix_cursor, 0);
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::TapeInvalid)
        );
        error
    };
    for (payload, expected) in cases {
        assert_eq!(tag_error::<CanonProbe>(Err(reject(payload))), expected);
    }
    let bool_error = reject(bad_bool);
    assert!(bool_error
        .source()
        .and_then(|cause| cause.downcast_ref::<postcard::Error>())
        .is_some());
}

#[test]
fn blocked_account_query_does_not_serialize_execution() {
    use std::sync::{mpsc, Barrier, Mutex, OnceLock};
    use std::time::Duration;
    static GATES: OnceLock<(Barrier, Barrier)> = OnceLock::new();
    fn hold() {
        let gates = GATES.get().expect("account-query gates");
        gates.0.wait();
        gates.1.wait();
    }
    let gates = GATES.get_or_init(|| (Barrier::new(2), Barrier::new(2)));
    let context = witness_context(7);
    context
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .expect("offer material"),
        )
        .expect("activate");
    let leader = context.attestation_public();
    let (completed, completion) = mpsc::channel();
    std::thread::scope(|scope| {
        let query_context = std::sync::Arc::clone(&context);
        let query = scope.spawn(move || {
            let _runtime = install_context(query_context);
            let _pause = pause_account_query_for_test(hold);
            try_derive_sealed_account_keys(
                Ledger::Gratis,
                alloy_primitives::Address::repeat_byte(2),
                [3; 32],
                [0; 65],
            )
        });
        gates.0.wait();
        let consensus_context = std::sync::Arc::clone(&context);
        scope.spawn(move || {
            use outbe_tee::witness::{
                EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
            };
            let _runtime = install_context(consensus_context);
            let state = std::sync::Arc::new(Mutex::new(WitnessState::new(
                WitnessExecutionMode::ProposerRecord,
                Some(leader),
                EnclaveWitnessTapeV1::default(),
            )));
            let _scope = WitnessExecutionScope::enter(std::sync::Arc::clone(&state));
            let recorded = consensus_witness_call(
                CallMeta::recorded("signed_witness"),
                {
                    let body = signed_body(1, B256::repeat_byte(0x5a));
                    move |resident| sign_body(resident, body)
                },
                authenticate_body,
            );
            assert!(state.lock().expect("witness").staged.len() == 1);
            completed.send(recorded).expect("consensus result");
        });
        let execution = completion.recv_timeout(Duration::from_secs(5));
        gates.1.wait();
        assert!(execution
            .expect("consensus finished while the account query stayed blocked")
            .is_ok());
        let error = query
            .join()
            .expect("account query")
            .expect("runtime installed")
            .expect_err("invalid owner signature");
        assert!(
            matches!(error, TransportError::EnclaveCause { .. }) && !error.is_node_local(),
            "{error}"
        );
    });
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PromisLocal {
    n: u8,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

impl WitnessSchema for PromisLocal {
    const TAG: WitnessResponseTag = WitnessResponseTag::PromisOpApplied;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}

#[test]
fn proposer_record_local_decode_failure_is_local_unavailable() {
    use outbe_tee::witness::{
        BlockWitnessFault, EnclaveWitnessTapeV1, TypedWitnessPayload, WitnessExecutionMode,
        WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};
    let context = witness_context(4);
    let leader = context.attestation_public();
    let _guard = install_context(Arc::clone(&context));
    let inputs = B256::repeat_byte(0x5a);
    let clean = PromisLocal {
        n: 1,
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    };
    let mut trailing = encode_tagged_payload(WitnessResponseTag::PromisOpApplied, &clean).unwrap();
    trailing.push(0x00);
    let unstripped = encode_tagged_payload(
        WitnessResponseTag::PromisOpApplied,
        &PromisLocal {
            attestation_tag: vec![9],
            ..clean.clone()
        },
    )
    .unwrap();
    let mut overlong = encode_tagged_payload(WitnessResponseTag::PromisOpApplied, &clean).unwrap();
    let length_at = overlong.len() - 1;
    assert_eq!(overlong[length_at], 0);
    overlong.splice(length_at..length_at + 1, [0x80, 0x00]);
    let hash_mismatch = encode_tagged_payload(
        WitnessResponseTag::PromisOpApplied,
        &PromisLocal {
            inputs_canonical_hash: B256::repeat_byte(0x11),
            ..clean.clone()
        },
    )
    .unwrap();
    let local = |payload: Vec<u8>, expected: WitnessTagError| {
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::ProposerRecord,
            Some(leader),
            EnclaveWitnessTapeV1::default(),
        )));
        state.lock().expect("witness").tx_index = Some(0);
        let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
        let error = outbe_tee::tee_consensus_typed_call(
            CallMeta::recorded("local_result"),
            {
                let clean = clean.clone();
                move |_| Ok(clean)
            },
            move |_| {
                Ok(TypedWitnessPayload {
                    payload,
                    inputs_canonical_hash: inputs,
                    attestation_tag: vec![7, 7],
                })
            },
            |key, wire| {
                super::decode_domain_wire(key, wire, &|_: &[u8; 32], _: &PromisLocal| {
                    Err(TransportError::UnexpectedResponse)
                })
            },
        )
        .expect_err("local payload");
        let witness = state.lock().expect("witness");
        assert_eq!(
            witness.first_witness_fault,
            Some(BlockWitnessFault::LocalUnavailable)
        );
        assert!(witness.staged.is_empty());
        assert_eq!(tag_error::<PromisLocal>(Err(error)), expected);
    };
    local(trailing, WitnessTagError::TrailingBytes);
    local(unstripped, WitnessTagError::InnerAttestationTag);
    local(overlong, WitnessTagError::NonCanonical);
    local(hash_mismatch, WitnessTagError::InputsHashMismatch);
    local(vec![3], WitnessTagError::NonRecordable { tag: 3 });
    local(vec![6], WitnessTagError::NonRecordable { tag: 6 });

    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    state.lock().expect("witness").tx_index = Some(0);
    let _scope = WitnessExecutionScope::enter(Arc::clone(&state));
    let corrupt = consensus_witness_call(
        CallMeta::recorded("local_result"),
        |resident| {
            let mut body = sign_body(resident, signed_body(11, inputs))?;
            body.attestation_tag[0] ^= 0xff;
            Ok(body)
        },
        authenticate_body,
    )
    .expect_err("corrupt attestation tag");
    let witness = state.lock().expect("witness");
    assert_eq!(
        witness.first_witness_fault,
        Some(BlockWitnessFault::LocalUnavailable)
    );
    assert!(witness.staged.is_empty());
    assert!(matches!(corrupt, TransportError::AppAttestation { .. }));
}
