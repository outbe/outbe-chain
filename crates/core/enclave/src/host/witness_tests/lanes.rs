//! Lane and error compatibility regressions.
use super::*;

#[test]
fn payload_lane_isolation_enforces_account_query_canary_and_execution_matrix() {
    let signer = SigningKey::from_bytes(&[0x93; 32]);
    let pubkey = signer.verifying_key().to_bytes();
    let observed_lanes = Arc::new(Mutex::new(Vec::new()));
    let lanes_clone = Arc::clone(&observed_lanes);

    let server = spawn_mock_dev_enclave(pubkey, move |lane, req| {
        lanes_clone.lock().unwrap().push((lane, req.label()));
        match req {
            EnclaveRequest::DeriveAccountKeys { account, .. } => {
                Ok(EnclaveResponse::AccountKeysSealed {
                    account,
                    sealed: vec![0xAA; 80],
                    nonce: [0xBB; 12],
                    enclave_ephemeral_pubkey: [0xCC; 32],
                })
            }
            EnclaveRequest::ProcessTributeOfferBatch { .. } => {
                let hash = B256::repeat_byte(0x41);
                let pre = crate::protocol::tribute_offer_attestation_preimage(hash, &[]).unwrap();
                Ok(EnclaveResponse::TributeOfferBatch {
                    results: Vec::new(),
                    inputs_canonical_hash: hash,
                    attestation_tag: signer.sign(&pre).to_bytes().to_vec(),
                })
            }
            EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { .. } => {
                let hash = B256::repeat_byte(0x42);
                let pre =
                    crate::tribute_v2::encrypted_offer_attestation_preimage(hash, &[]).unwrap();
                Ok(EnclaveResponse::EncryptedTributeOfferBatchV2 {
                    results: Vec::new(),
                    inputs_canonical_hash: hash,
                    attestation_tag: signer.sign(&pre).to_bytes().to_vec(),
                })
            }
            other => Err(format!("unexpected request {}", other.label())),
        }
    });
    let client = EnclaveClient::connect_endpoint(&server.endpoint).unwrap();
    let _ = outbe_tee::install_enclave_client(client, server.endpoint.clone());

    let derive = EnclaveRequest::DeriveAccountKeys {
        ledger: Ledger::Gratis,
        account: Address::repeat_byte(0x11),
        requester_ephemeral_pubkey: [0x22; 32],
        owner_sig: vec![0x33; 65],
    };
    assert!(derive.encode_wire(PayloadLane::AccountQuery).is_ok());
    assert!(derive.encode_wire(PayloadLane::Execution).is_err());
    assert!(derive.encode_wire(PayloadLane::Canary).is_err());

    // Live try_domain_account_query accepts DeriveAccountKeys and rejects state-mutating / offer requests!
    let sealed = try_domain_account_query(&derive)
        .expect("installed enclave")
        .expect("DeriveAccountKeys allowed on AccountQuery");
    assert!(matches!(sealed, EnclaveResponse::AccountKeysSealed { .. }));

    let gratis_req = EnclaveRequest::ApplyGratisOp {
        request: Box::new(GratisOpRequest {
            op: GratisOp::Mint,
            chain_id: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            amount: U256::ONE,
            current_balance: Vec::new(),
            current_pledged: Vec::new(),
            modify_auth: sample_modify_auth(),
            fidelity: None,
        }),
    };
    let offer_v1 = EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() };
    let offer_v2 = EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers: Vec::new() };

    assert!(try_domain_account_query(&gratis_req).unwrap().is_err());
    assert!(try_domain_account_query(&offer_v2).unwrap().is_err());

    // Live canary_domain_request accepts offer batches on Canary and rejects ApplyGratisOp & DeriveAccountKeys!
    assert!(canary_domain_request(&offer_v1).is_ok());
    assert!(canary_domain_request(&offer_v2).is_ok());
    assert!(canary_domain_request(&gratis_req).is_err());
    assert!(canary_domain_request(&derive).is_err());

    let exec_only = EnclaveRequest::SnapshotFidelityLeagues {
        request: Box::new(FidelitySnapshotRequest {
            timestamp: 1_700_000_000,
            first_qualified_start: 20250115,
            entries: Vec::new(),
        }),
    };
    assert!(exec_only.encode_wire(PayloadLane::Execution).is_ok());
    assert!(canary_domain_request(&exec_only).is_err());
    assert!(try_domain_account_query(&exec_only).unwrap().is_err());

    let lanes = observed_lanes.lock().unwrap().clone();
    assert_eq!(
        lanes,
        vec![
            (PayloadLane::AccountQuery, "derive_account_keys"),
            (PayloadLane::Canary, "process_tribute_offer_batch"),
            (
                PayloadLane::Canary,
                "process_encrypted_tribute_offer_batch_v2"
            ),
        ]
    );
}

#[test]
fn metric_classes_and_local_crypto_error_messages_remain_exact() {
    let bad_sig = || TeeAttestationError::InvalidSignatureLength { actual: 3 };
    for (descriptor, expected) in [
        (TRIBUTE_OFFER_ATTESTATION, "tribute_offer_attestation"),
        (GRATIS_OP_ATTESTATION, "gratis_op_attestation"),
        (PROMIS_OP_ATTESTATION, "promis_op_attestation"),
        (FIDELITY_ATTESTATION, "fidelity_attestation"),
    ] {
        assert_eq!(
            TransportError::app_attestation(descriptor, bad_sig()).metric_class(),
            expected
        );
    }
    for (descriptor, expected) in [
        (TRIBUTE_SOURCE_REJECTED, "tribute_source_rejected"),
        (NOD_MINT_REJECTED, "nod_mint_rejected"),
    ] {
        assert_eq!(
            TransportError::app_rejected(descriptor, "rejected").metric_class(),
            expected
        );
    }

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
