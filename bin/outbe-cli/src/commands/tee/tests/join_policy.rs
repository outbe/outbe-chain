use super::*;

#[test]
fn join_transport_uses_node_host_for_sgx_without_dcap() {
    assert_eq!(
        select_join_transport(AttestationMode::DcapRequired, true).unwrap(),
        JoinTransport::AuthorizedNodeHost
    );
    assert!(select_join_transport(AttestationMode::DcapRequired, false).is_err());
    assert_eq!(
        select_join_transport(AttestationMode::GramineDirectDev, true).unwrap(),
        JoinTransport::AuthorizedNodeHost
    );
    assert_eq!(
        select_join_transport(AttestationMode::GramineDirectDev, false).unwrap(),
        JoinTransport::Development
    );
}

#[test]
fn join_classifies_resident_offer_key_before_relay() {
    let expected_offer_pub = [0x41; 32];
    let public_keys = |offer_key_ready, recipient_x25519_pub| EnclaveResponse::PublicKeys {
        offer_key_ready,
        recipient_x25519_pub,
        attestation_pub: [0x42; 32],
        noise_static_pub: [0x43; 32],
        tee_bls_pub: vec![0x44; 48],
        dkg_enc_pub: [0x45; 32],
        dkg_enc_sig: vec![0x46; 96],
    };

    assert_eq!(
        classify_join_offer_key_state(public_keys(true, expected_offer_pub), expected_offer_pub,)
            .unwrap(),
        JoinOfferKeyState::ReadyExact
    );
    assert_eq!(
        classify_join_offer_key_state(public_keys(false, [0x47; 32]), expected_offer_pub).unwrap(),
        JoinOfferKeyState::Keyless
    );

    let mismatch =
        classify_join_offer_key_state(public_keys(true, [0x48; 32]), expected_offer_pub).unwrap();
    assert_eq!(mismatch, JoinOfferKeyState::ReadyMismatch);

    let unexpected =
        classify_join_offer_key_state(EnclaveResponse::Ack, expected_offer_pub).unwrap_err();
    assert!(unexpected
        .to_string()
        .contains("expected enclave PublicKeys"));
}

#[test]
fn join_completion_matrix_preserves_candidate_promotion_without_duplicate_ingest() {
    assert_eq!(
        plan_join_completion(JoinOfferKeyState::Keyless, false).unwrap(),
        JoinCompletionPlan {
            ingest_offer_key: true,
            promote_candidate: false,
        }
    );
    assert_eq!(
        plan_join_completion(JoinOfferKeyState::Keyless, true).unwrap(),
        JoinCompletionPlan {
            ingest_offer_key: true,
            promote_candidate: true,
        }
    );
    assert_eq!(
        plan_join_completion(JoinOfferKeyState::ReadyExact, false).unwrap(),
        JoinCompletionPlan {
            ingest_offer_key: false,
            promote_candidate: false,
        }
    );
    assert_eq!(
        plan_join_completion(JoinOfferKeyState::ReadyExact, true).unwrap(),
        JoinCompletionPlan {
            ingest_offer_key: false,
            promote_candidate: true,
        }
    );
    assert!(plan_join_completion(JoinOfferKeyState::ReadyMismatch, false).is_err());
}

#[tokio::test]
async fn finalized_durable_join_does_not_relay_a_second_transaction() {
    let raw_transaction = vec![0x62, 0x63];
    let transaction_hash = keccak256(&raw_transaction);
    let relay = ExactJoinRelayV1 {
        transaction_hash,
        raw_transaction,
    };
    let rpc = RecordingRpc::new([]);

    assert_eq!(
        relay_exact_join_transaction(&rpc, &relay, true)
            .await
            .unwrap(),
        format!("0x{}", hex::encode(transaction_hash))
    );
    rpc.assert_done();
    assert!(rpc.recorded_calls().is_empty());
}

#[tokio::test]
async fn pending_durable_join_relays_only_the_exact_raw_transaction() {
    let raw_transaction = vec![0x71, 0x72];
    let transaction_hash = keccak256(&raw_transaction);
    let relay = ExactJoinRelayV1 {
        transaction_hash,
        raw_transaction: raw_transaction.clone(),
    };
    let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
    let rpc = RecordingRpc::new([ExpectedRpcCall::ok(
        RecordedRpcCall::EthSendRawTransaction {
            raw_tx: raw_transaction,
        },
        RecordedRpcResponse::Text(encoded_hash.clone()),
    )]);

    assert_eq!(
        relay_exact_join_transaction(&rpc, &relay, false)
            .await
            .unwrap(),
        encoded_hash
    );
    rpc.assert_done();
}

#[tokio::test]
async fn already_known_durable_join_preserves_the_exact_transaction_identity() {
    let raw_transaction = vec![0x73, 0x74];
    let transaction_hash = keccak256(&raw_transaction);
    let relay = ExactJoinRelayV1 {
        transaction_hash,
        raw_transaction: raw_transaction.clone(),
    };
    let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
    let rpc = RecordingRpc::new([ExpectedRpcCall::err(
        RecordedRpcCall::EthSendRawTransaction {
            raw_tx: raw_transaction,
        },
        "already known",
    )]);

    assert_eq!(
        relay_exact_join_transaction(&rpc, &relay, false)
            .await
            .unwrap(),
        encoded_hash
    );
    rpc.assert_done();
}

#[tokio::test]
async fn nonce_too_low_is_accepted_only_for_the_exact_durable_join_receipt() {
    let raw_transaction = vec![0x75, 0x76];
    let transaction_hash = keccak256(&raw_transaction);
    let relay = ExactJoinRelayV1 {
        transaction_hash,
        raw_transaction: raw_transaction.clone(),
    };
    let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
    let rpc = RecordingRpc::new([
        ExpectedRpcCall::err(
            RecordedRpcCall::EthSendRawTransaction {
                raw_tx: raw_transaction,
            },
            "nonce too low",
        ),
        ExpectedRpcCall::ok(
            RecordedRpcCall::EthGetTransactionReceipt {
                transaction_hash: encoded_hash.clone(),
            },
            RecordedRpcResponse::OptionalValue(Some(serde_json::json!({
                "transactionHash": encoded_hash,
            }))),
        ),
    ]);

    assert_eq!(
        relay_exact_join_transaction(&rpc, &relay, false)
            .await
            .unwrap(),
        format!("0x{}", hex::encode(transaction_hash))
    );
    rpc.assert_done();
}

#[test]
fn committed_submission_rejects_a_different_global_evm_signer() {
    let original = Address::repeat_byte(0x41);
    let replacement = Address::repeat_byte(0x42);

    ensure_durable_join_registration_caller(Some(original), original).unwrap();
    ensure_durable_join_registration_caller(None, replacement).unwrap();
    assert!(
        ensure_durable_join_registration_caller(Some(original), replacement)
            .unwrap_err()
            .to_string()
            .contains("different global --private-key")
    );
}

#[test]
fn missing_committed_relay_is_only_recoverable_after_ready_exact_completion() {
    assert_eq!(
        plan_missing_committed_relay(true, JoinOfferKeyState::ReadyExact).unwrap(),
        MissingCommittedRelayPlan::CleanupReadyExact
    );
    assert_eq!(
        plan_missing_committed_relay(false, JoinOfferKeyState::Keyless).unwrap(),
        MissingCommittedRelayPlan::ConstructAndPersist
    );
    assert!(
        plan_missing_committed_relay(true, JoinOfferKeyState::Keyless)
            .unwrap_err()
            .to_string()
            .contains("no durable pre-relay transaction checkpoint")
    );
    assert!(
        plan_missing_committed_relay(false, JoinOfferKeyState::ReadyMismatch)
            .unwrap_err()
            .to_string()
            .contains("does not match finalized TeeRegistry")
    );
}

#[test]
fn expired_rejoin_uses_exact_next_binding_and_registration_versions() {
    let mut binding = test_renewal_binding();
    binding.binding_version = 7;
    binding.registration_version = 11;
    binding.renewal_nonce = 5;
    binding.transition_nonce = 3;
    assert_eq!(
        registration_counters(Some(&binding)).unwrap(),
        (8, 12, 5, 3)
    );
    assert_eq!(registration_counters(None).unwrap(), (1, 0, 0, 0));
}

#[test]
fn tee_join_rejects_a_live_binding_but_accepts_the_deadline_as_expired() {
    let mut binding = test_renewal_binding();
    binding.valid_until = 200;
    assert!(ensure_joinable_binding(Some(&binding), 199).is_err());
    assert!(ensure_joinable_binding(Some(&binding), 200).is_ok());
    assert!(ensure_joinable_binding(Some(&binding), 201).is_ok());
    assert!(ensure_joinable_binding(None, 199).is_ok());
}

fn test_renewal_binding() -> outbe_operator::tee::RenewalBindingV1 {
    outbe_operator::tee::RenewalBindingV1 {
        node_id_hash: B256::repeat_byte(1),
        enclave_id: B256::repeat_byte(2),
        binding_id: B256::repeat_byte(3),
        intent_hash: B256::repeat_byte(4),
        evidence_hash: B256::repeat_byte(5),
        policy_hash: B256::repeat_byte(6),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        lease_started_at: 100,
        valid_until: 200,
        collateral_valid_until: 300,
        recipient_x25519: B256::repeat_byte(7),
        attestation_ed25519: B256::repeat_byte(8),
        noise_responder_x25519: B256::repeat_byte(9),
        mrenclave: B256::repeat_byte(10),
        mrsigner: B256::repeat_byte(11),
        isv_prod_id: 1,
        isv_svn: 1,
        platform_tcb_status: 0,
        verdict_hash: B256::repeat_byte(12),
        node_host_authorization_hash: B256::repeat_byte(13),
    }
}
