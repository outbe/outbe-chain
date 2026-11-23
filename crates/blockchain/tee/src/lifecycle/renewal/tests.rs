use super::*;

// Lease expiry must route to paid rejoin, never another renew transaction.
#[test]
fn schedule_preserves_half_lease_window_and_exclusive_expiry() {
    for (now, expected) in [
        (
            99,
            Some(RenewalStep::NotDue {
                opens_at_timestamp: 100,
            }),
        ),
        (100, None),
        (199, None),
        (200, Some(RenewalStep::LeaseAlreadyExpiredNeedsRejoin)),
        (201, Some(RenewalStep::LeaseAlreadyExpiredNeedsRejoin)),
    ] {
        assert_eq!(renewal_schedule_v1(now, 200, 200).unwrap(), expected);
    }
    for lease in [0, 3] {
        assert!(renewal_schedule_v1(10, 200, lease).is_err());
    }
}

pub(crate) mod support;
use crate::lifecycle::*;
use alloy_consensus::{transaction::RlpEcdsaDecodableTx as _, TxEip1559};
use alloy_sol_types::SolCall as _;
use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};
use support::*;

#[tokio::test]
async fn zero_balance_renewal_is_durable_zero_fee_exact_replay_and_nonce_rebuild() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let first = renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    assert!(matches!(
        first,
        RenewalStep::Submitted {
            replayed: false,
            ..
        }
    ));
    let saved = journal.load().unwrap().unwrap();
    let signed = TxEip1559::rlp_decode_signed(&mut &saved.raw_signed_tx[1..]).unwrap();
    let tx = signed.tx();
    assert_eq!(
        (tx.max_priority_fee_per_gas, tx.value, tx.max_fee_per_gas),
        (0, alloy_primitives::U256::ZERO, 17)
    );
    assert_eq!(tx.gas_limit, renewal_gas_limit_v1().unwrap());
    let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(&tx.input).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence).unwrap();
    assert_eq!(
        (
            evidence.intent().registration_version,
            evidence.intent().renewal_nonce,
            evidence.intent().requested_valid_until
        ),
        (2, 1, 14400)
    );
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::Submitted { replayed: true, .. }
    ));
    {
        let sent = chain.sent.lock().unwrap();
        assert_eq!(sent[0], sent[1]);
    }
    chain.nonces = [4, 5, 5];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::AwaitingFinality
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    chain.nonces = [5, 6, 7];
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::Submitted {
            replayed: false,
            ..
        }
    ));
    assert_eq!(journal.load().unwrap().unwrap().nonce, 7);
    chain.timestamp(7200);
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::LeaseAlreadyExpiredNeedsRejoin
    );
    assert_eq!(chain.sent.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn finalized_renewal_is_not_duplicated_and_identity_policy_guards_precede_signing() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let record = journal.load().unwrap().unwrap();
    chain.binding().registration_version = 2;
    chain.binding().renewal_nonce = 1;
    chain.binding().valid_until = 14400;
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::NotDue {
            opens_at_timestamp: 10800
        }
    );
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::Completed
    );
    chain.binding().recipient_x25519 = alloy_primitives::B256::repeat_byte(99);
    assert!(matches!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await,
        Err(RenewalError::BindingMismatch)
    ));
    assert_eq!(record.nonce, 4);
}

#[tokio::test]
async fn upgrade_phases_preserve_prepared_source_renewal_and_exact_promoted_exception() {
    use alloy_primitives::B256;
    for phase in [
        RenewalUpgradePhaseV1::Idle,
        RenewalUpgradePhaseV1::CandidatePrepared,
        RenewalUpgradePhaseV1::KeyProvisioned,
        RenewalUpgradePhaseV1::CandidateKeyReady,
        RenewalUpgradePhaseV1::SubmissionPrepared,
        RenewalUpgradePhaseV1::Submitted,
        RenewalUpgradePhaseV1::Finalized,
        RenewalUpgradePhaseV1::TerminalMissedCutoff,
    ] {
        let root = tempfile::tempdir().unwrap();
        let live = context(9);
        let signer = Signer::default();
        let mut chain = Chain::new(view(&live), root.path());
        if phase == RenewalUpgradePhaseV1::Idle {
            chain.view.checkpoint.base_fee_per_gas = alloy_primitives::U256::ZERO;
        }
        chain.view.candidate = Some(LifecycleUpgradeCandidateV1 {
            context_hash: B256::repeat_byte(8),
            valid_until: 10000,
            source_binding_id: chain.view.binding.as_ref().unwrap().binding_id,
            target_hash: B256::repeat_byte(9),
            nonce: 1,
        });
        let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
        let outcome = renew_once_v1(&chain, &signer, &live, &mut journal, phase)
            .await
            .unwrap();
        if matches!(
            phase,
            RenewalUpgradePhaseV1::Idle | RenewalUpgradePhaseV1::CandidatePrepared
        ) {
            assert!(matches!(outcome, RenewalStep::Submitted { .. }));
            if phase == RenewalUpgradePhaseV1::Idle {
                let record = journal.load().unwrap().unwrap();
                let signed = TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
                assert_eq!(signed.tx().max_fee_per_gas, 7);
            }
        } else {
            assert_eq!(outcome, RenewalStep::BlockedByActiveUpgrade);
            assert!(journal.load().unwrap().is_none());
            assert!(chain.sent.lock().unwrap().is_empty());
        }
    }
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let policy = chain.view.binding.as_ref().unwrap().policy_hash;
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Promoted {
                candidate_manifest_hash: B256::ZERO,
                successor_policy_hash: policy
            }
        )
        .await
        .unwrap(),
        RenewalStep::BlockedByActiveUpgrade
    );
    let phase = RenewalUpgradePhaseV1::Promoted {
        candidate_manifest_hash: live.manifest().authorization_hash().unwrap(),
        successor_policy_hash: policy,
    };
    assert!(matches!(
        renew_once_v1(&chain, &signer, &live, &mut journal, phase)
            .await
            .unwrap(),
        RenewalStep::Submitted { .. }
    ));
}

#[tokio::test]
async fn expired_pending_renew_waits_without_replay_then_releases_same_identity_rejoin_slot() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let original = journal.load().unwrap().unwrap();
    chain.timestamp(7200);
    chain.nonces = [4, 5, 5];
    let before = std::fs::read(&chain.journal_path).unwrap();
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::AwaitingFinality
    );
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    chain.nonces = [4, 4, 4];
    assert_eq!(
        renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle
        )
        .await
        .unwrap(),
        RenewalStep::LeaseAlreadyExpiredNeedsRejoin
    );
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::RebuildRequired
    );
    use super::prepare::{evidence_v1, signed_record, target_v1, ServiceCallV1};
    use alloy_primitives::B256;
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live, signer.address())
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign {
            nonce: original.nonce
        }
    );
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live, signer.address())
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign {
            nonce: original.nonce
        }
    );
    let mut raw = &original.raw_signed_tx[1..];
    let signed = TxEip1559::rlp_decode_signed(&mut raw).unwrap();
    let previous = ITeeRegistryV1::renewEnclaveCall::abi_decode(&signed.tx().input).unwrap();
    let mut intent = AttestationEvidenceV1::decode_canonical(&previous.evidence)
        .unwrap()
        .intent()
        .clone();
    intent.operation = AttestationOperationV1::RegisterEnclave;
    intent.binding_version += 1;
    intent.binding_id = B256::repeat_byte(55);
    intent.requested_valid_until = 14400;
    let (evidence, signature) =
        evidence_v1::<std::io::Error, k256::ecdsa::Error>(&live, &intent, &chain.view.policy, 7200)
            .unwrap();
    let node = signer
        .sign_node_hash(intent.intent_hash().unwrap())
        .unwrap();
    let calldata = ITeeRegistryV1::registerEnclaveCall {
        validatorNodeBinding: Default::default(),
        validatorSignature: Default::default(),
        nodeBindingSignature: Default::default(),
        evidence: evidence.into(),
        nodeSignature: node.to_vec().into(),
        enclaveSignature: signature.to_vec().into(),
    }
    .abi_encode();
    let rejoin = signed_record::<Chain, Signer>(
        &signer,
        chain.view.checkpoint,
        original.nonce,
        ServiceCallV1 {
            gas_limit: 1_000_000,
            calldata,
            kind: ServiceTxKindV1::Register,
            intent: ServiceTxIntentV1 {
                target: target_v1(&intent).unwrap(),
                candidate: None,
            },
        },
    )
    .unwrap();
    journal.begin(rejoin.clone()).unwrap();
    assert_eq!(journal.load().unwrap(), Some(rejoin));
}

// A completed onboarding journal remains the last record until the next service begin.
#[tokio::test]
async fn finalized_onboarding_allows_renewal_only_after_the_resident_key_is_imported() {
    for (kind, ready) in [
        (ServiceTxKindV1::Register, true),
        (ServiceTxKindV1::UpgradeTransition, true),
        (ServiceTxKindV1::Register, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let live = context(9);
        let signer = Signer::default();
        if ready {
            import_test_key(&live);
        }
        let mut chain = Chain::new(view(&live), root.path());
        let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
        finalized_onboarding(&mut chain, &live, &signer, &mut journal, kind);
        let result = renew_once_v1(
            &chain,
            &signer,
            &live,
            &mut journal,
            RenewalUpgradePhaseV1::Idle,
        )
        .await;
        if ready {
            assert!(
                matches!(
                    result,
                    Ok(RenewalStep::Submitted {
                        replayed: false,
                        ..
                    })
                ),
                "{kind:?}: {result:?}"
            );
            assert_eq!(
                journal.load().unwrap().unwrap().kind,
                ServiceTxKindV1::Renew
            );
            assert_eq!(journal.load().unwrap().unwrap().nonce, 4);
            assert_eq!(chain.sent.lock().unwrap().len(), 1);
        } else {
            assert!(matches!(result, Err(RenewalError::PendingOtherOperation)));
            assert!(chain.sent.lock().unwrap().is_empty());
            assert_eq!(journal.load().unwrap().unwrap().kind, kind);
        }
    }
}

#[tokio::test]
async fn pending_renewal_abi_error_retains_its_typed_source() {
    use alloy_consensus::SignableTransaction as _;
    use std::error::Error as _;
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();

    // Corrupt calldata inside a validly signed EIP-1559 envelope on disk.
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&chain.journal_path).unwrap()).unwrap();
    let tx = TxEip1559 {
        chain_id: chain.view.checkpoint.chain_id,
        nonce: 4,
        gas_limit: 1_000_000,
        max_fee_per_gas: 17,
        max_priority_fee_per_gas: 0,
        to: alloy_primitives::TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        value: alloy_primitives::U256::ZERO,
        input: vec![0].into(),
        access_list: Default::default(),
    };
    let sig = signer.sign_transaction_hash(tx.signature_hash()).unwrap();
    let mut raw = vec![2];
    alloy_consensus::transaction::RlpEcdsaEncodableTx::rlp_encode_signed(&tx, &sig, &mut raw);
    json["record"]["tx_hash"] = serde_json::to_value(alloy_primitives::keccak256(&raw)).unwrap();
    json["record"]["raw_signed_tx"] =
        serde_json::to_value(alloy_primitives::Bytes::from(raw)).unwrap();
    let corrupted = serde_json::to_vec(&json).unwrap();
    std::fs::write(&chain.journal_path, &corrupted).unwrap();

    let error = renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        &error,
        RenewalError::Journal(ServiceTxJournalError::CallDecode(_))
    ));
    assert!(error
        .source()
        .is_some_and(|cause| cause.is::<alloy_sol_types::Error>()));
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), corrupted);
}

#[tokio::test]
async fn phase1_renew_load_rejects_tampered_calldata_signatures_and_target_preserving_bytes() {
    use alloy_consensus::SignableTransaction as _;

    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let valid_record = journal.load().unwrap().unwrap();
    let valid_signed = TxEip1559::rlp_decode_signed(&mut &valid_record.raw_signed_tx[1..]).unwrap();
    let valid_call =
        ITeeRegistryV1::renewEnclaveCall::abi_decode(&valid_signed.tx().input).unwrap();

    let resign = |input: Vec<u8>, target_valid_until: u64| -> Vec<u8> {
        let mut tx = valid_signed.tx().clone();
        tx.input = input.into();
        let sig = signer.sign_transaction_hash(tx.signature_hash()).unwrap();
        let mut raw = vec![2];
        alloy_consensus::transaction::RlpEcdsaEncodableTx::rlp_encode_signed(&tx, &sig, &mut raw);
        let mut rec = valid_record.clone();
        rec.tx_hash = alloy_primitives::keccak256(&raw);
        rec.raw_signed_tx = raw.into();
        rec.public_intent.target.valid_until = target_valid_until;
        serde_json::to_vec(&serde_json::json!({ "version": 1, "record": rec })).unwrap()
    };

    // 1. Target metadata diverges from signed calldata.
    let target_mismatch = resign(valid_signed.tx().input.to_vec(), 99_999);
    // 2. Non-canonical trailing bytes on calldata.
    let mut trailing = valid_signed.tx().input.to_vec();
    trailing.push(0x00);
    let non_canonical = resign(trailing, valid_record.public_intent.target.valid_until);
    // 3. Corrupted node signature.
    let mut bad_node = valid_call.clone();
    let mut node_bytes = bad_node.nodeSignature.to_vec();
    node_bytes[0] ^= 0xFF;
    bad_node.nodeSignature = node_bytes.into();
    let bad_node_sig = resign(
        bad_node.abi_encode(),
        valid_record.public_intent.target.valid_until,
    );
    // 4. Corrupted enclave signature.
    let mut bad_enclave = valid_call.clone();
    let mut enc_bytes = bad_enclave.enclaveSignature.to_vec();
    enc_bytes[0] ^= 0xFF;
    bad_enclave.enclaveSignature = enc_bytes.into();
    let bad_enclave_sig = resign(
        bad_enclave.abi_encode(),
        valid_record.public_intent.target.valid_until,
    );
    // 5. Corrupted NoAttestationDev dev_signature inside evidence.
    let AttestationEvidenceV1::NoAttestationDev(mut dev) =
        AttestationEvidenceV1::decode_canonical(&valid_call.evidence).unwrap()
    else {
        panic!("expected NoAttestationDev");
    };
    let valid_intent = dev.intent.clone();
    dev.dev_signature[0] ^= 0xFF;
    let mut bad_dev_call = valid_call.clone();
    bad_dev_call.evidence = AttestationEvidenceV1::NoAttestationDev(dev)
        .encode_canonical()
        .unwrap()
        .into();
    let bad_dev_sig = resign(
        bad_dev_call.abi_encode(),
        valid_record.public_intent.target.valid_until,
    );

    let resign_intent = |intent: RegistrationIntentV1| -> Vec<u8> {
        let enclave_sig = live.sign_domain_attestation(intent.intent_hash().unwrap().as_slice());
        let node_sig = signer
            .sign_node_hash(intent.intent_hash().unwrap())
            .unwrap();
        let evidence = AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
            intent: intent.clone(),
            dev_attestation_public: intent.attestation_ed25519,
            dev_signature: enclave_sig,
            transition_key_ready_proof: None,
        })
        .encode_canonical()
        .unwrap();
        let calldata = ITeeRegistryV1::renewEnclaveCall {
            evidence: evidence.into(),
            nodeSignature: node_sig.to_vec().into(),
            enclaveSignature: enclave_sig.to_vec().into(),
        }
        .abi_encode();
        let mut tx = valid_signed.tx().clone();
        tx.input = calldata.into();
        let sig = signer.sign_transaction_hash(tx.signature_hash()).unwrap();
        let mut raw = vec![2];
        alloy_consensus::transaction::RlpEcdsaEncodableTx::rlp_encode_signed(&tx, &sig, &mut raw);
        let mut rec = valid_record.clone();
        rec.tx_hash = alloy_primitives::keccak256(&raw);
        rec.raw_signed_tx = raw.into();
        rec.public_intent.target = super::prepare::target_v1(&intent).unwrap();
        serde_json::to_vec(&serde_json::json!({ "version": 1, "record": rec })).unwrap()
    };

    // 6. derived_enclave_id != enclave_id (validly re-signed).
    let mut wrong_enclave_id = valid_intent.clone();
    wrong_enclave_id.enclave_id = alloy_primitives::B256::repeat_byte(0xAA);
    let derived_enclave_mismatch = resign_intent(wrong_enclave_id);

    // 7. operation != RenewEnclave (validly re-signed).
    let mut wrong_op = valid_intent.clone();
    wrong_op.operation = AttestationOperationV1::RegisterEnclave;
    let operation_mismatch = resign_intent(wrong_op);

    // 8. intent.chain_id != finalized_checkpoint.chain_id (validly re-signed).
    let mut wrong_chain = valid_intent.clone();
    wrong_chain.chain_id = alloy_primitives::U256::from(999).to_be_bytes();
    let chain_id_mismatch = resign_intent(wrong_chain);

    // 9. intent.genesis_hash != finalized_checkpoint.genesis_hash (validly re-signed).
    let mut wrong_genesis = valid_intent;
    wrong_genesis.genesis_hash = alloy_primitives::B256::repeat_byte(0xCC);
    let genesis_mismatch = resign_intent(wrong_genesis);

    for (label, bytes, expect_invalid_tx) in [
        ("target_mismatch", target_mismatch, false),
        ("non_canonical", non_canonical, true),
        ("bad_node_sig", bad_node_sig, false),
        ("bad_enclave_sig", bad_enclave_sig, false),
        ("bad_dev_sig", bad_dev_sig, false),
        ("derived_enclave_mismatch", derived_enclave_mismatch, false),
        ("operation_mismatch", operation_mismatch, false),
        ("chain_id_mismatch", chain_id_mismatch, false),
        ("genesis_mismatch", genesis_mismatch, false),
    ] {
        std::fs::write(&chain.journal_path, &bytes).unwrap();
        let load_err = journal.load().unwrap_err();
        if expect_invalid_tx {
            assert!(
                matches!(load_err, ServiceTxJournalError::InvalidTransaction),
                "{label}: expected InvalidTransaction, got {load_err:?}"
            );
        } else {
            assert!(
                matches!(load_err, ServiceTxJournalError::InvalidIntent),
                "{label}: expected InvalidIntent, got {load_err:?}"
            );
        }
        assert!(
            matches!(
                renew_once_v1(
                    &chain,
                    &signer,
                    &live,
                    &mut journal,
                    RenewalUpgradePhaseV1::Idle,
                )
                .await,
                Err(RenewalError::Journal(_))
            ),
            "{label}: renew_once_v1 must fail closed"
        );
        assert_eq!(
            std::fs::read(&chain.journal_path).unwrap(),
            bytes,
            "{label}: journal.json must be preserved byte-for-byte"
        );
    }
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn phase2_diverged_pending_renew_fails_before_replay_while_finalized_renew_reconciles() {
    let root = tempfile::tempdir().unwrap();
    let live = context(9);
    let signer = Signer::default();
    let mut chain = Chain::new(view(&live), root.path());
    let mut journal = ServiceTxJournalV1::open(root.path()).unwrap();
    renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    let before = std::fs::read(&chain.journal_path).unwrap();

    // Diverged live binding (!mutation_applied, nonce free -> ReplayExact) fails Phase-2 validate_pending.
    chain.binding().registration_version = 6;
    chain.binding().renewal_nonce = 5;
    let err = renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RenewalError::PendingRenewDiverged));
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(&chain.journal_path).unwrap(), before);

    // Already-finalized Renew on restart (T13): mutation_applied == true reconciles to Completed
    // without calling validate_pending and without false failure.
    chain.binding().registration_version = 2;
    chain.binding().renewal_nonce = 1;
    chain.binding().valid_until = 14400;
    let outcome = renew_once_v1(
        &chain,
        &signer,
        &live,
        &mut journal,
        RenewalUpgradePhaseV1::Idle,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        RenewalStep::NotDue {
            opens_at_timestamp: 10800
        }
    );
    let completed = journal.load().unwrap().unwrap();
    assert_eq!(completed.state, ServiceTxStateV1::Completed);
    assert_eq!(chain.sent.lock().unwrap().len(), 1);
}
