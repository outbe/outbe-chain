//! Actual resident engine parity through all three witness execution modes.
use super::*;
use crate::host::domain_in_process::{dispatch_payload, tests::context};
use outbe_tee::{
    client_global::set_in_process_tee_context_for_test, in_process::InProcessTeeContext,
    resident::ResidentOfferMaterial,
};

fn ready(seed: u8) -> Arc<InProcessTeeContext> {
    let context = context(seed);
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    context
}
fn auth(
    request: &EnclaveRequest,
    key: &[u8; 32],
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    match (request, response) {
        (
            EnclaveRequest::ApplyGratisOp { request },
            EnclaveResponse::GratisOpApplied { result },
        ) => verify_gratis_op_attestation(
            key,
            gratis_op_canonical_hash(request),
            result,
            &result.attestation_tag,
        ),
        (
            EnclaveRequest::ApplyPromisOp { request },
            EnclaveResponse::PromisOpApplied { result },
        ) => verify_promis_op_attestation(
            key,
            promis_op_canonical_hash(request),
            result,
            &result.attestation_tag,
        ),
        (
            EnclaveRequest::SnapshotFidelityLeagues { request },
            EnclaveResponse::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash: _,
                attestation_tag,
            },
        ) => super::super::verify_fidelity_snapshot_attestation(
            key,
            crate::protocol::fidelity_snapshot_canonical_hash(request),
            leagues,
            attestation_tag,
        ),
        (
            EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers },
            EnclaveResponse::EncryptedTributeOfferBatchV2 {
                results,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = crate::tribute_v2::encrypted_offer_inputs_hash(777, offers);
            assert_eq!(*inputs_canonical_hash, expected);
            crate::tribute_v2::verify_attestation(
                key,
                &crate::tribute_v2::encrypted_offer_attestation_preimage(expected, results)?,
                attestation_tag,
            )
        }
        (
            EnclaveRequest::QueryFidelityIndex { request },
            EnclaveResponse::FidelityQueryRejected {
                inputs_canonical_hash,
                reason,
                attestation_tag,
            },
        ) => outbe_tee::verify_signed_rejection(
            key,
            outbe_tee::SignedRejectionRef {
                domain: b"outbe/fidelity/query-rejected/v1",
                expected_inputs_hash: fidelity_query_canonical_hash(request),
                actual_inputs_hash: *inputs_canonical_hash,
                reason,
                tag: attestation_tag,
            },
        )
        .map_err(|error| TransportError::app_attestation(FIDELITY_ATTESTATION, error)),
        _ => authenticate_request(request, key, response),
    }
}
#[test]
fn resident_proposer_enclave_verifier_and_offline_replay_have_identical_witness_bytes() {
    let _policy = PolicyReset;
    let proposer = ready(7);
    let verifier = ready(8);
    let chain_id = B256::from(proposer.manifest().chain_id);
    let account = Address::repeat_byte(0x22);
    let gratis_key = proposer
        .with_offer_material(|material| {
            crate::gratis::derive_gratis_state_key(material, chain_id, 0)
        })
        .unwrap()
        .unwrap();
    let gratis_account = crate::gratis::derive_account_keys(&gratis_key, account).unwrap();
    let gratis_auth = ModifyAuth {
        mac: crate::gratis::modify_mac(
            &gratis_account,
            &crate::gratis::ModifyOperation {
                account,
                op: GratisOp::Mint,
                amount: U256::ONE,
                op_nonce: 0,
                chain_id,
            },
        ),
        op_nonce: 0,
    };
    let promis_key = proposer
        .with_offer_material(|material| {
            crate::promis::derive_promis_state_key(material, chain_id, 0)
        })
        .unwrap()
        .unwrap();
    let promis_account = crate::promis::derive_account_keys(&promis_key, account).unwrap();
    let promis_auth = ModifyAuth {
        mac: crate::promis::modify_mac(
            &promis_account,
            &crate::promis::ModifyOperation {
                account,
                op: PromisOp::Mint,
                amount: U256::ONE,
                op_nonce: 0,
                chain_id,
            },
        ),
        op_nonce: 0,
    };
    let nod_key = proposer
        .with_offer_material(|material| {
            crate::nod_encryption::derive_nod_state_key(material, chain_id, 0)
        })
        .unwrap()
        .unwrap();
    let nod = crate::nod_encryption::encrypt_nod(
        &nod_key,
        NodTermsV1 {
            chain_id: 777,
            nod_id: U256::ONE,
            owner: account,
            emission_day: 20250115,
            league_id: 2,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 840,
        },
        U256::ONE,
    )
    .unwrap();
    let requests = [
        EnclaveRequest::ApplyGratisOp {
            request: Box::new(GratisOpRequest {
                op: GratisOp::Mint,
                chain_id,
                account,
                amount: U256::ONE,
                current_balance: Vec::new(),
                current_pledged: Vec::new(),
                modify_auth: gratis_auth,
                fidelity: None,
            }),
        },
        EnclaveRequest::ApplyPromisOp {
            request: Box::new(PromisOpRequest {
                op: PromisOp::Mint,
                chain_id,
                account,
                amount: U256::ONE,
                current_balance: Vec::new(),
                modify_auth: promis_auth,
            }),
        },
        EnclaveRequest::SnapshotFidelityLeagues {
            request: Box::new(FidelitySnapshotRequest {
                timestamp: 1_700_000_000,
                first_qualified_start: 20250115,
                entries: Vec::new(),
            }),
        },
        EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers: Vec::new() },
        EnclaveRequest::QueryFidelityIndex {
            request: Box::new(FidelityQueryRequest {
                chain_id,
                account,
                cohort_blob: Vec::new(),
                query_timestamp: 100,
                block_timestamp: 100,
                first_qualified_start: 20250115,
                expiry: 200,
                owner_sig: vec![0; 65],
            }),
        },
        EnclaveRequest::MineEncryptedNodV1 {
            request: Box::new(MineEncryptedNodRequestV1 {
                nod,
                current_balance: Vec::new(),
                modify_auth: sample_modify_auth(),
                fidelity: sample_fidelity_section(),
            }),
        },
        EnclaveRequest::ApplyTributeDayOpV2 {
            request: Box::new(crate::tribute_day::TributeDayOpRequestV2 {
                chain_id: 777,
                worldwide_day: crate::protocol::WorldwideDay::from(20250115u32),
                previous: None,
                public_state_hash: B256::ZERO,
                operation: crate::tribute_day::TributeDayOperationV2::AdjustTransient {
                    nominal_amount_minor: U256::ONE,
                    add: true,
                },
            }),
        },
    ];
    for request in requests {
        let leader = proposer.attestation_public();
        set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
        let record = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::ProposerRecord,
            Some(leader),
            EnclaveWitnessTapeV1::default(),
        )));
        record.lock().unwrap().tx_index = Some(0);
        let expected = {
            let _runtime = set_in_process_tee_context_for_test(Some((
                Arc::clone(&proposer),
                dispatch_payload,
            )));
            let _execution = enter_execution_witness(Arc::clone(&record));
            consensus_domain_call(
                request.label(),
                &request,
                || TransportError::UnexpectedResponse,
                |key, response| auth(&request, key, response),
            )
            .unwrap_or_else(|error| panic!("{}: {error:?}", request.label()))
        };
        match &expected.1 {
            EnclaveResponse::GratisOpApplied { result } => {
                assert!(matches!(result.status, GratisOpStatus::Applied))
            }
            EnclaveResponse::PromisOpApplied { result } => {
                assert!(matches!(result.status, PromisOpStatus::Applied))
            }
            _ => {}
        }
        let entry = record.lock().unwrap().staged[0].clone();
        let wire_request = request.encode_wire(PayloadLane::Execution).unwrap();
        let WireRequest::Payload { lane, payload } = wire_request else {
            unreachable!()
        };
        let direct = dispatch_payload(&proposer, lane, &payload).unwrap();
        assert_eq!(EnclaveWitnessEntryV1::try_from(direct).unwrap(), entry);
        let tape = EnclaveWitnessTapeV1 {
            prefix: Vec::new(),
            txs: vec![TxWitnessV1 {
                tx_index: 0,
                entries: vec![entry],
            }],
        };
        for mode in [
            WitnessExecutionMode::VerifierWithEnclave,
            WitnessExecutionMode::VerifierReplay,
        ] {
            let replay = mode == WitnessExecutionMode::VerifierReplay;
            set_witness_policy_for_test(Some(if replay {
                WitnessPolicy::ReplayOnly
            } else {
                WitnessPolicy::EnclaveBacked
            }));
            let state = Arc::new(Mutex::new(WitnessState::new(
                mode,
                Some(leader),
                tape.clone(),
            )));
            state.lock().unwrap().tx_index = Some(0);
            let _runtime = set_in_process_tee_context_for_test(if replay {
                None
            } else {
                Some((Arc::clone(&verifier), dispatch_payload))
            });
            let _execution = enter_execution_witness(Arc::clone(&state));
            let actual = consensus_domain_call(
                request.label(),
                &request,
                || TransportError::UnexpectedResponse,
                |key, response| auth(&request, key, response),
            )
            .unwrap_or_else(|error| panic!("{}: {error:?}", request.label()));
            assert_eq!(actual, expected);
            assert_eq!(state.lock().unwrap().first_witness_fault, None);
        }
    }
}

#[test]
fn unsigned_resident_domain_refusals_reject_candidates_without_aborting_proposal() {
    use outbe_tee::{witness::CandidateRejection, CallMeta};
    let _policy = PolicyReset;
    set_witness_policy_for_test(Some(WitnessPolicy::EnclaveBacked));
    let context = ready(7);
    let leader = context.attestation_public();
    let _runtime =
        set_in_process_tee_context_for_test(Some((Arc::clone(&context), dispatch_payload)));
    let request = EnclaveRequest::ApplyTributeDayOpV2 {
        request: Box::new(crate::tribute_day::TributeDayOpRequestV2 {
            chain_id: 778,
            worldwide_day: 20250115u32.into(),
            previous: None,
            public_state_hash: B256::ZERO,
            operation: crate::tribute_day::TributeDayOperationV2::Freeze,
        }),
    };
    let payload = postcard::to_allocvec(&request).unwrap();
    // A valid leader entry lets the verifier reach the local-failure classification seam.
    let leader_payload =
        postcard::to_allocvec(&EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 {
            offers: Vec::new(),
        })
        .unwrap();
    let leader_entry = EnclaveWitnessEntryV1::try_from(
        dispatch_payload(&context, PayloadLane::Execution, &leader_payload).unwrap(),
    )
    .unwrap();
    for (lane, keyless) in [
        (PayloadLane::Execution, false),
        (PayloadLane::AccountQuery, false),
        (PayloadLane::Execution, true),
    ] {
        for mode in [
            WitnessExecutionMode::ProposerRecord,
            WitnessExecutionMode::VerifierWithEnclave,
        ] {
            let _keyless_runtime = keyless.then(|| {
                set_in_process_tee_context_for_test(Some((
                    crate::host::domain_in_process::tests::context(9),
                    dispatch_payload,
                )))
            });
            let state = Arc::new(Mutex::new(WitnessState::new(
                mode,
                Some(leader),
                EnclaveWitnessTapeV1 {
                    prefix: Vec::new(),
                    txs: vec![TxWitnessV1 {
                        tx_index: 0,
                        entries: vec![leader_entry.clone()],
                    }],
                },
            )));
            state.lock().unwrap().tx_index = Some(0);
            state.lock().unwrap().is_user_candidate = true;
            let _execution = enter_execution_witness(Arc::clone(&state));
            let error = outbe_tee::tee_consensus_call(
                CallMeta::recorded(request.label()),
                &WireRequest::Payload {
                    lane,
                    payload: payload.clone(),
                },
                || TransportError::UnexpectedResponse,
                |_, _| Ok(()),
            )
            .unwrap_err();
            if keyless {
                assert!(matches!(
                    error,
                    TransportError::InProcess(outbe_tee::InProcessDispatchError::Resident(_))
                ));
            } else {
                assert!(matches!(
                    error,
                    TransportError::InProcess(
                        outbe_tee::InProcessDispatchError::Domain(_)
                            | outbe_tee::InProcessDispatchError::LaneDenied { .. }
                    )
                ));
            }
            let state = state.lock().unwrap();
            let proposer = mode == WitnessExecutionMode::ProposerRecord;
            assert_eq!(
                state.candidate_rejection,
                (proposer && !keyless).then_some(CandidateRejection::UnsignedEnclaveError)
            );
            assert_eq!(
                state.first_witness_fault,
                (!proposer || keyless).then_some(BlockWitnessFault::LocalUnavailable)
            );
            assert!(state.staged.is_empty());
        }
    }
}
