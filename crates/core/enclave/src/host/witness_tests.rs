//! Regression tests for canonical domain `Payload` witness recording, two-key
//! verifier cross-checking, lane isolation, and error/metric compatibility.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use ed25519_dalek::{Signer, SigningKey};
use outbe_core_common::nod_encryption::{EncryptedNodV1, NodTermsV1};
use outbe_tee::{
    call_context::{ContextScope, EnclaveCallContextV1, EnclaveContextKindV1},
    protocol::{EnclaveResponse as WireResponse, PayloadLane},
    witness::{
        set_witness_policy_for_test, BlockWitnessFault, EnclaveWitnessEntryV1,
        EnclaveWitnessTapeV1, TxWitnessV1, WitnessExecutionMode, WitnessExecutionScope,
        WitnessPolicy, WitnessState,
    },
    TeeAttestationError, TransportError,
};

use super::{
    authenticate::authenticate_request,
    consensus_domain_call,
    errors::{EnclaveClientError, LocalCryptoError, TributeSourceError},
    nod_mine::{self, MineEncryptedNodRequestV1, MineEncryptedNodResultV1},
    protocol::{
        fidelity_query_attestation_preimage, fidelity_query_canonical_hash,
        gratis_op_attestation_preimage, gratis_op_canonical_hash,
        mine_gratis_from_promis_attestation_preimage, mine_gratis_from_promis_canonical_hash,
        promis_op_attestation_preimage, promis_op_canonical_hash, EnclaveRequest, EnclaveResponse,
        FidelityCohortOp, FidelityOpOutcome, FidelityOpSection, FidelityQueryRequest,
        FidelityQueryResult, GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus, Ledger,
        MineGratisFromPromisRequestV1, MineGratisFromPromisResultV1, MineGratisFromPromisStatusV1,
        ModifyAuth, PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus,
    },
    tribute_client::set_test_local_enclave_hook,
    verification::{
        verify_fidelity_query_attestation, verify_gratis_op_attestation,
        verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation,
    },
};

struct PolicyReset;

impl Drop for PolicyReset {
    fn drop(&mut self) {
        set_witness_policy_for_test(None);
        set_test_local_enclave_hook(None);
    }
}

struct ExecutionWitnessGuard {
    _ctx: ContextScope,
    _witness: WitnessExecutionScope,
}

fn enter_execution_witness(handle: Arc<Mutex<WitnessState>>) -> ExecutionWitnessGuard {
    ExecutionWitnessGuard {
        _ctx: ContextScope::enter(EnclaveCallContextV1 {
            kind: EnclaveContextKindV1::Execution,
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            block_number: 10,
            block_timestamp: 1_700_000_000,
            protocol_version: 1,
        }),
        _witness: WitnessExecutionScope::enter(handle),
    }
}

fn sample_modify_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0x44; 32],
        op_nonce: 3,
    }
}

fn sample_fidelity_section() -> FidelityOpSection {
    FidelityOpSection {
        op: FidelityCohortOp::In,
        timestamp: 1_700_000_000,
        first_qualified_start: 1_699_000_000,
        current_blob: vec![0xaa, 0xbb],
    }
}

fn record_and_verify_across_two_keys(
    request: EnclaveRequest,
    sign_response: impl Fn(&SigningKey) -> EnclaveResponse + Send + Sync + 'static,
    authenticate: impl Fn(&[u8; 32], &EnclaveResponse) -> Result<(), TransportError> + Copy,
) -> EnclaveWitnessEntryV1 {
    let _guard = PolicyReset;
    set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));

    let leader_sk = SigningKey::from_bytes(&[0x21; 32]);
    let leader_pub = leader_sk.verifying_key().to_bytes();
    let verifier_sk = SigningKey::from_bytes(&[0x42; 32]);
    let verifier_pub = verifier_sk.verifying_key().to_bytes();
    assert_ne!(leader_pub, verifier_pub);

    let sign_arc = Arc::new(sign_response);

    // 1. ProposerRecord with leader_sk
    let sign_leader = Arc::clone(&sign_arc);
    let leader_sk_clone = leader_sk.clone();
    set_test_local_enclave_hook(Some(Arc::new(move |_| {
        Ok((leader_pub, sign_leader(&leader_sk_clone)))
    })));

    let proposer_state = Arc::new(Mutex::new({
        let mut ws = WitnessState::new(
            WitnessExecutionMode::ProposerRecord,
            Some(leader_pub),
            EnclaveWitnessTapeV1::default(),
        );
        ws.tx_index = Some(0);
        ws
    }));
    let (proposer_key, proposer_resp) = {
        let _ctx = enter_execution_witness(Arc::clone(&proposer_state));
        consensus_domain_call(
            request.label(),
            &request,
            || TransportError::Unavailable("missing".into()),
            authenticate,
        )
        .expect("proposer record must succeed")
    };
    assert_eq!(proposer_key, leader_pub);
    authenticate(&proposer_key, &proposer_resp).expect("downstream caller re-verifies tag");

    let recorded_entry = proposer_state
        .lock()
        .unwrap()
        .staged
        .first()
        .cloned()
        .expect("one staged witness entry");
    let EnclaveWitnessEntryV1::Payload {
        payload,
        attestation_tag,
        ..
    } = &recorded_entry
    else {
        panic!("expected EnclaveWitnessEntryV1::Payload");
    };
    assert_eq!(attestation_tag.len(), 64);

    // Inner payload must have empty attestation_tag!
    let (raw_decoded, rest) = postcard::take_from_bytes::<EnclaveResponse>(payload).unwrap();
    assert!(rest.is_empty());
    assert!(
        raw_decoded.attestation_tag().is_empty(),
        "inner attestation_tag must be stripped inside canonical Payload.payload"
    );

    let tape = EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: vec![TxWitnessV1 {
            tx_index: 0,
            entries: vec![recorded_entry.clone()],
        }],
    };

    // 2. VerifierWithEnclave using DIFFERENT key `verifier_sk` -> MUST match semantically!
    let sign_verifier = Arc::clone(&sign_arc);
    set_test_local_enclave_hook(Some(Arc::new(move |_| {
        Ok((verifier_pub, sign_verifier(&verifier_sk)))
    })));

    let verifier_state = Arc::new(Mutex::new({
        let mut ws = WitnessState::new(
            WitnessExecutionMode::VerifierWithEnclave,
            Some(leader_pub),
            tape.clone(),
        );
        ws.tx_index = Some(0);
        ws
    }));
    let (verified_key, verified_resp) = {
        let _ctx = enter_execution_witness(Arc::clone(&verifier_state));
        consensus_domain_call(
            request.label(),
            &request,
            || TransportError::Unavailable("missing".into()),
            authenticate,
        )
        .expect("verifier with distinct enclave key must match semantically")
    };
    assert_eq!(verified_key, leader_pub);
    assert_eq!(verified_resp, proposer_resp);
    assert_eq!(verifier_state.lock().unwrap().first_witness_fault, None);

    // 3. VerifierReplay without ANY local enclave hook -> MUST replay cleanly!
    set_witness_policy_for_test(Some(WitnessPolicy::ReplayOnly));
    set_test_local_enclave_hook(Some(Arc::new(|_| {
        panic!("VerifierReplay must never invoke local enclave transport")
    })));
    let replay_state = Arc::new(Mutex::new({
        let mut ws =
            WitnessState::new(WitnessExecutionMode::VerifierReplay, Some(leader_pub), tape);
        ws.tx_index = Some(0);
        ws
    }));
    let (replay_key, replay_resp) = {
        let _ctx = enter_execution_witness(Arc::clone(&replay_state));
        consensus_domain_call(
            request.label(),
            &request,
            || TransportError::Unavailable("missing".into()),
            authenticate,
        )
        .expect("verifier replay must succeed without sidecar")
    };
    assert_eq!(replay_key, leader_pub);
    assert_eq!(replay_resp, proposer_resp);
    assert_eq!(replay_state.lock().unwrap().first_witness_fault, None);

    recorded_entry
}

#[test]
fn nested_tag_consensus_structs_record_and_verify_across_distinct_enclave_keys() {
    // 1. GratisOpResult
    let gratis_req = GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: B256::repeat_byte(0x01),
        account: Address::repeat_byte(0x11),
        amount: U256::from(500u64),
        current_balance: vec![1, 2],
        current_pledged: Vec::new(),
        modify_auth: sample_modify_auth(),
        fidelity: None,
    };
    let gratis_hash = gratis_op_canonical_hash(&gratis_req);
    record_and_verify_across_two_keys(
        EnclaveRequest::ApplyGratisOp {
            request: Box::new(gratis_req),
        },
        move |sk| {
            let mut result = GratisOpResult {
                status: GratisOpStatus::Applied,
                new_balance: vec![9, 9, 9],
                new_pledged: Vec::new(),
                event_amount: U256::from(500u64),
                next_op_nonce: 4,
                fidelity: None,
                inputs_canonical_hash: gratis_hash,
                attestation_tag: Vec::new(),
                encrypted_event_amount: vec![7, 7],
            };
            let preimage = gratis_op_attestation_preimage(gratis_hash, &result).unwrap();
            result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
            EnclaveResponse::GratisOpApplied {
                result: Box::new(result),
            }
        },
        |key, resp| {
            let EnclaveResponse::GratisOpApplied { result } = resp else {
                return Err(TransportError::UnexpectedResponse);
            };
            verify_gratis_op_attestation(
                key,
                result.inputs_canonical_hash,
                result,
                &result.attestation_tag,
            )
        },
    );

    // 2. PromisOpResult
    let promis_req = PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: B256::repeat_byte(0x02),
        account: Address::repeat_byte(0x22),
        amount: U256::from(750u64),
        current_balance: Vec::new(),
        modify_auth: sample_modify_auth(),
    };
    let promis_hash = promis_op_canonical_hash(&promis_req);
    record_and_verify_across_two_keys(
        EnclaveRequest::ApplyPromisOp {
            request: Box::new(promis_req),
        },
        move |sk| {
            let mut result = PromisOpResult {
                status: PromisOpStatus::Applied,
                new_balance: vec![3, 4, 5],
                event_amount: U256::from(750u64),
                next_op_nonce: 4,
                inputs_canonical_hash: promis_hash,
                attestation_tag: Vec::new(),
                encrypted_event_amount: vec![8, 8],
            };
            let preimage = promis_op_attestation_preimage(promis_hash, &result).unwrap();
            result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
            EnclaveResponse::PromisOpApplied {
                result: Box::new(result),
            }
        },
        |key, resp| {
            let EnclaveResponse::PromisOpApplied { result } = resp else {
                return Err(TransportError::UnexpectedResponse);
            };
            verify_promis_op_attestation(
                key,
                result.inputs_canonical_hash,
                result,
                &result.attestation_tag,
            )
        },
    );

    // 3. MineGratisFromPromisResultV1
    let mine_gp_req = MineGratisFromPromisRequestV1 {
        chain_id: B256::repeat_byte(0x03),
        account: Address::repeat_byte(0x33),
        encrypted_promis_amount: vec![0xaa; 92],
        current_promis_balance: vec![1],
        promis_modify_auth: sample_modify_auth(),
        current_gratis_balance: vec![2],
        gratis_modify_auth: sample_modify_auth(),
        fidelity: sample_fidelity_section(),
    };
    let mine_gp_hash = mine_gratis_from_promis_canonical_hash(&mine_gp_req);
    record_and_verify_across_two_keys(
        EnclaveRequest::MineGratisFromPromisV1 {
            request: Box::new(mine_gp_req),
        },
        move |sk| {
            let mut result = MineGratisFromPromisResultV1 {
                status: MineGratisFromPromisStatusV1::Applied,
                new_promis_balance: vec![10],
                encrypted_promis_event_amount: vec![11],
                next_promis_op_nonce: 4,
                new_gratis_balance: vec![12],
                encrypted_gratis_event_amount: vec![13],
                next_gratis_op_nonce: 4,
                fidelity: Some(FidelityOpOutcome {
                    new_blob: vec![14],
                    qualified_start_initialized: None,
                    league: 2,
                }),
                inputs_canonical_hash: mine_gp_hash,
                attestation_tag: Vec::new(),
            };
            let preimage =
                mine_gratis_from_promis_attestation_preimage(mine_gp_hash, &result).unwrap();
            result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
            EnclaveResponse::GratisMinedFromPromisV1 {
                result: Box::new(result),
            }
        },
        |key, resp| {
            let EnclaveResponse::GratisMinedFromPromisV1 { result } = resp else {
                return Err(TransportError::UnexpectedResponse);
            };
            verify_mine_gratis_from_promis_attestation(
                key,
                result.inputs_canonical_hash,
                result,
                &result.attestation_tag,
            )
        },
    );

    // 4. FidelityQueryResult
    let fq_req = FidelityQueryRequest {
        chain_id: B256::repeat_byte(0x04),
        account: Address::repeat_byte(0x44),
        cohort_blob: vec![5, 6],
        query_timestamp: 1_700_000_000,
        block_timestamp: 1_700_000_000,
        first_qualified_start: 1_699_000_000,
        expiry: 1_700_003_600,
        owner_sig: vec![0x55; 65],
    };
    let fq_hash = fidelity_query_canonical_hash(&fq_req);
    record_and_verify_across_two_keys(
        EnclaveRequest::QueryFidelityIndex {
            request: Box::new(fq_req),
        },
        move |sk| {
            let mut result = FidelityQueryResult {
                rcfi: U256::from(123_456u64),
                efficiency: U256::from(999_000u64),
                league: 5,
                inputs_canonical_hash: fq_hash,
                attestation_tag: Vec::new(),
            };
            let preimage = fidelity_query_attestation_preimage(fq_hash, &result).unwrap();
            result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
            EnclaveResponse::FidelityIndexQueried {
                result: Box::new(result),
            }
        },
        |key, resp| {
            let EnclaveResponse::FidelityIndexQueried { result } = resp else {
                return Err(TransportError::UnexpectedResponse);
            };
            verify_fidelity_query_attestation(
                key,
                result.inputs_canonical_hash,
                result,
                &result.attestation_tag,
            )
        },
    );

    // 5. MineEncryptedNodResultV1
    let nod_req = MineEncryptedNodRequestV1 {
        nod: EncryptedNodV1 {
            terms: NodTermsV1 {
                chain_id: 31_337,
                nod_id: U256::from(77u64),
                owner: Address::repeat_byte(0x55),
                emission_day: 20_260_101,
                league_id: 3,
                entry_price_minor: U256::from(1_000_000u64),
                issuance_currency: 840,
                reference_currency: 840,
            },
            encryption_binding: B256::repeat_byte(0x99),
            encrypted_gratis_amount: vec![0u8; 56],
        },
        current_balance: vec![1, 2],
        modify_auth: sample_modify_auth(),
        fidelity: sample_fidelity_section(),
    };
    let nod_hash = nod_mine::inputs_hash(&nod_req).unwrap();
    let nod_enclave_req = EnclaveRequest::MineEncryptedNodV1 {
        request: Box::new(nod_req),
    };
    let nod_req_for_auth = nod_enclave_req.clone();
    record_and_verify_across_two_keys(
        nod_enclave_req,
        move |sk| {
            let mut result = MineEncryptedNodResultV1 {
                new_balance: vec![21, 22],
                next_op_nonce: 4,
                fidelity: FidelityOpOutcome {
                    new_blob: vec![23],
                    qualified_start_initialized: None,
                    league: 3,
                },
                inputs_canonical_hash: nod_hash,
                attestation_tag: Vec::new(),
                encrypted_event_amount: vec![24],
            };
            let preimage = nod_mine::result_preimage(&result).unwrap();
            result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
            EnclaveResponse::EncryptedNodMinedV1 {
                result: Box::new(result),
            }
        },
        |key, resp| authenticate_request(&nod_req_for_auth, key, resp),
    );
}

#[test]
fn semantic_divergence_between_proposer_and_verifier_latches_semantic_mismatch() {
    let _guard = PolicyReset;
    set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));

    let leader_sk = SigningKey::from_bytes(&[0x31; 32]);
    let leader_pub = leader_sk.verifying_key().to_bytes();
    let verifier_sk = SigningKey::from_bytes(&[0x32; 32]);
    let verifier_pub = verifier_sk.verifying_key().to_bytes();

    let req = PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: B256::repeat_byte(0x05),
        account: Address::repeat_byte(0x66),
        amount: U256::from(100u64),
        current_balance: Vec::new(),
        modify_auth: sample_modify_auth(),
    };
    let hash = promis_op_canonical_hash(&req);
    let enclave_req = EnclaveRequest::ApplyPromisOp {
        request: Box::new(req),
    };

    let make_signed = |sk: &SigningKey, new_balance: Vec<u8>| {
        let mut result = PromisOpResult {
            status: PromisOpStatus::Applied,
            new_balance,
            event_amount: U256::from(100u64),
            next_op_nonce: 4,
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
            encrypted_event_amount: Vec::new(),
        };
        let preimage = promis_op_attestation_preimage(hash, &result).unwrap();
        result.attestation_tag = sk.sign(&preimage).to_bytes().to_vec();
        EnclaveResponse::PromisOpApplied {
            result: Box::new(result),
        }
    };

    let leader_wire = make_signed(&leader_sk, vec![0xaa])
        .into_wire_response()
        .unwrap();
    let leader_entry = EnclaveWitnessEntryV1::try_from(leader_wire).unwrap();

    // Verifier produces valid signature under verifier_sk, EXCEPT `new_balance` differs!
    let verifier_resp = make_signed(&verifier_sk, vec![0xbb]);
    set_test_local_enclave_hook(Some(Arc::new(move |_| {
        Ok((verifier_pub, verifier_resp.clone()))
    })));

    let verifier_state = Arc::new(Mutex::new({
        let mut ws = WitnessState::new(
            WitnessExecutionMode::VerifierWithEnclave,
            Some(leader_pub),
            EnclaveWitnessTapeV1 {
                prefix: Vec::new(),
                txs: vec![TxWitnessV1 {
                    tx_index: 0,
                    entries: vec![leader_entry],
                }],
            },
        );
        ws.tx_index = Some(0);
        ws
    }));
    let err = {
        let _ctx = enter_execution_witness(Arc::clone(&verifier_state));
        consensus_domain_call(
            enclave_req.label(),
            &enclave_req,
            || TransportError::Unavailable("missing".into()),
            |key, resp| {
                let EnclaveResponse::PromisOpApplied { result } = resp else {
                    return Err(TransportError::UnexpectedResponse);
                };
                verify_promis_op_attestation(
                    key,
                    result.inputs_canonical_hash,
                    result,
                    &result.attestation_tag,
                )
            },
        )
        .unwrap_err()
    };
    assert!(err.to_string().contains("SemanticMismatch"));
    assert_eq!(
        verifier_state.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::SemanticMismatch)
    );
}

#[test]
fn non_canonical_payload_bytes_latch_tape_invalid_on_replay_and_verifier() {
    let _guard = PolicyReset;
    let leader_sk = SigningKey::from_bytes(&[0x71; 32]);
    let leader_pub = leader_sk.verifying_key().to_bytes();

    let req = PromisOpRequest {
        op: PromisOp::Mint,
        chain_id: B256::repeat_byte(0x06),
        account: Address::repeat_byte(0x77),
        amount: U256::from(50u64),
        current_balance: Vec::new(),
        modify_auth: sample_modify_auth(),
    };
    let hash = promis_op_canonical_hash(&req);
    let enclave_req = EnclaveRequest::ApplyPromisOp {
        request: Box::new(req),
    };

    let mut clean_result = PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: vec![1],
        event_amount: U256::from(50u64),
        next_op_nonce: 4,
        inputs_canonical_hash: hash,
        attestation_tag: Vec::new(),
        encrypted_event_amount: Vec::new(),
    };
    let preimage = promis_op_attestation_preimage(hash, &clean_result).unwrap();
    let sig = leader_sk.sign(&preimage).to_bytes().to_vec();

    // Case A: trailing byte appended to canonical payload
    let mut trailing_payload = postcard::to_allocvec(&EnclaveResponse::PromisOpApplied {
        result: Box::new(clean_result.clone()),
    })
    .unwrap();
    trailing_payload.push(0x00);

    // Case B: non-empty inner attestation_tag left inside payload
    clean_result.attestation_tag = sig.clone();
    let unstripped_payload = postcard::to_allocvec(&EnclaveResponse::PromisOpApplied {
        result: Box::new(clean_result),
    })
    .unwrap();

    for bad_payload in [trailing_payload, unstripped_payload] {
        let bad_entry = EnclaveWitnessEntryV1::Payload {
            payload: bad_payload,
            inputs_canonical_hash: hash,
            attestation_tag: sig.clone(),
        };
        for mode in [
            WitnessExecutionMode::VerifierReplay,
            WitnessExecutionMode::VerifierWithEnclave,
        ] {
            set_witness_policy_for_test(Some(if mode == WitnessExecutionMode::VerifierReplay {
                WitnessPolicy::ReplayOnly
            } else {
                WitnessPolicy::EnclaveBacked
            }));
            let state = Arc::new(Mutex::new({
                let mut ws = WitnessState::new(
                    mode,
                    Some(leader_pub),
                    EnclaveWitnessTapeV1 {
                        prefix: Vec::new(),
                        txs: vec![TxWitnessV1 {
                            tx_index: 0,
                            entries: vec![bad_entry.clone()],
                        }],
                    },
                );
                ws.tx_index = Some(0);
                ws
            }));
            let res = {
                let _ctx = enter_execution_witness(Arc::clone(&state));
                consensus_domain_call(
                    enclave_req.label(),
                    &enclave_req,
                    || TransportError::Unavailable("missing".into()),
                    |_, _| Ok(()),
                )
            };
            assert!(res.is_err());
            assert_eq!(
                state.lock().unwrap().first_witness_fault,
                Some(BlockWitnessFault::TapeInvalid)
            );
        }
    }
}

#[test]
fn non_recordable_signed_lysis_response_verifies_on_wire_but_never_enters_witness_tape() {
    let signer = SigningKey::from_bytes(&[0x81; 32]);
    let pubkey = signer.verifying_key().to_bytes();
    let inputs_hash = B256::repeat_byte(0x91);
    let result = super::lysis::LysisResult {
        nods: Vec::new(),
        total_nominal_minor: U256::from(1_000u64),
        unused_lysis_limit_minor: U256::from(200u64),
    };
    let preimage = super::lysis::result_preimage(inputs_hash, &result).unwrap();
    let tag = signer.sign(&preimage).to_bytes().to_vec();

    let domain_resp = EnclaveResponse::LysisExecuted {
        result: Box::new(result),
        inputs_canonical_hash: inputs_hash,
        attestation_tag: tag,
    };
    assert!(!domain_resp.is_recordable());

    let wire = domain_resp.clone().into_wire_response().unwrap();
    assert!(matches!(
        wire,
        WireResponse::Payload {
            recordable: false,
            ..
        }
    ));
    // Non-recordable Payload MUST fail conversion into EnclaveWitnessEntryV1!
    assert!(EnclaveWitnessEntryV1::try_from(wire.clone()).is_err());

    // Round-trips cleanly on direct session call and preserves Ed25519 signature verification
    let decoded = EnclaveResponse::from_wire_response(&wire).unwrap();
    assert_eq!(decoded, domain_resp);
    let EnclaveResponse::LysisExecuted {
        result: decoded_res,
        inputs_canonical_hash: decoded_hash,
        attestation_tag: decoded_tag,
    } = decoded
    else {
        panic!("expected LysisExecuted");
    };
    let verify_preimage = super::lysis::result_preimage(decoded_hash, &decoded_res).unwrap();
    super::tribute_v2::verify_attestation(&pubkey, &verify_preimage, &decoded_tag).unwrap();

    // Tampered input hash fails verification
    let wrong_preimage =
        super::lysis::result_preimage(B256::repeat_byte(0x92), &decoded_res).unwrap();
    assert!(super::tribute_v2::verify_attestation(&pubkey, &wrong_preimage, &decoded_tag).is_err());
}

#[test]
fn payload_lane_isolation_enforces_account_query_canary_and_execution_matrix() {
    let derive = EnclaveRequest::DeriveAccountKeys {
        ledger: Ledger::Gratis,
        account: Address::repeat_byte(0x11),
        requester_ephemeral_pubkey: [0x22; 32],
        owner_sig: vec![0x33; 65],
    };
    assert!(derive.encode_wire(PayloadLane::AccountQuery).is_ok());
    assert!(derive.encode_wire(PayloadLane::Execution).is_err());
    assert!(derive.encode_wire(PayloadLane::Canary).is_err());

    let offer_v1 = EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() };
    let offer_v2 = EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers: Vec::new() };
    for offer_req in [&offer_v1, &offer_v2] {
        assert!(offer_req.encode_wire(PayloadLane::Execution).is_ok());
        assert!(offer_req.encode_wire(PayloadLane::Canary).is_ok());
        assert!(offer_req.encode_wire(PayloadLane::AccountQuery).is_err());
    }

    let discard = EnclaveRequest::DiscardNodTransferV1 { id: B256::ZERO };
    assert!(discard.encode_wire(PayloadLane::Execution).is_ok());
    assert!(discard.encode_wire(PayloadLane::Canary).is_err());
    assert!(discard.encode_wire(PayloadLane::AccountQuery).is_err());
}

#[test]
fn metric_classes_and_local_crypto_error_messages_remain_exact() {
    let bad_sig = || TeeAttestationError::InvalidSignatureLength { actual: 3 };
    assert_eq!(
        TransportError::payload_attestation("offer", "tribute_offer_attestation", bad_sig())
            .metric_class(),
        "tribute_offer_attestation"
    );
    assert_eq!(
        TransportError::payload_attestation("gratis-op", "gratis_op_attestation", bad_sig())
            .metric_class(),
        "gratis_op_attestation"
    );
    assert_eq!(
        TransportError::payload_attestation("promis-op", "promis_op_attestation", bad_sig())
            .metric_class(),
        "promis_op_attestation"
    );
    assert_eq!(
        TransportError::payload_attestation("fidelity", "fidelity_attestation", bad_sig())
            .metric_class(),
        "fidelity_attestation"
    );

    let source_err = TransportError::from(EnclaveClientError::TributeSourceRejected(
        TributeSourceError::ContextMismatch,
    ));
    assert_eq!(source_err.metric_class(), "tribute_source_rejected");
    assert_eq!(
        source_err.to_string(),
        "Tribute source rejected: Tribute source context mismatch"
    );

    let mint_err = TransportError::from(EnclaveClientError::NodMintRejected {
        reason: "bad nonce".into(),
    });
    assert_eq!(mint_err.metric_class(), "nod_mint_rejected");
    assert_eq!(
        mint_err.to_string(),
        "encrypted NOD mint rejected: bad nonce"
    );

    let expected_crypto_messages = [
        (
            LocalCryptoError::OfferEncryptionRngFailure,
            "offer encryption rng failure",
        ),
        (LocalCryptoError::HkdfExpand, "hkdf expand"),
        (LocalCryptoError::HkdfFill, "hkdf fill"),
        (LocalCryptoError::AeadKey, "aead key"),
        (LocalCryptoError::AeadSeal, "aead seal"),
        (
            LocalCryptoError::InvalidFidelityCohortEnvelope,
            "invalid encrypted Fidelity cohort envelope",
        ),
        (
            LocalCryptoError::InvalidFidelityViewKey,
            "invalid Fidelity view key",
        ),
        (
            LocalCryptoError::FidelityCohortDecryptionFailed,
            "Fidelity cohort decryption failed",
        ),
        (
            LocalCryptoError::InvalidGratisBalance,
            "invalid encrypted Gratis balance",
        ),
        (
            LocalCryptoError::InvalidGratisViewKey,
            "invalid Gratis view key",
        ),
        (
            LocalCryptoError::GratisBalanceDecryptionFailed,
            "Gratis balance decryption failed",
        ),
        (
            LocalCryptoError::InvalidGratisAmountLength,
            "invalid Gratis amount length",
        ),
        (
            LocalCryptoError::InvalidPromisBalance,
            "invalid encrypted Promis balance",
        ),
        (
            LocalCryptoError::InvalidPromisViewKey,
            "invalid Promis view key",
        ),
        (
            LocalCryptoError::PromisBalanceDecryptionFailed,
            "Promis balance decryption failed",
        ),
        (
            LocalCryptoError::InvalidPromisAmountLength,
            "invalid Promis amount length",
        ),
        (
            LocalCryptoError::InvalidNodEncoding,
            "invalid encrypted NOD encoding",
        ),
        (
            LocalCryptoError::InvalidNodAmountKey,
            "invalid NOD amount key",
        ),
        (
            LocalCryptoError::NodAmountDecryptionFailed,
            "NOD amount decryption failed",
        ),
        (
            LocalCryptoError::InvalidNodAmountLength,
            "invalid NOD amount length",
        ),
        (
            LocalCryptoError::InvalidTributeEncoding,
            "invalid encrypted Tribute encoding",
        ),
        (
            LocalCryptoError::InvalidTributeAmountKey,
            "invalid Tribute amount key",
        ),
        (
            LocalCryptoError::TributeAmountDecryptionFailed,
            "Tribute amount decryption failed",
        ),
        (
            LocalCryptoError::InvalidTributeAmountLength,
            "invalid Tribute amount length",
        ),
    ];
    for (variant, expected) in expected_crypto_messages {
        assert_eq!(variant.as_str(), expected);
        assert_eq!(variant.to_string(), expected);
    }
}
