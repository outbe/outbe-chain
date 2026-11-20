use super::*;

#[tokio::test]
async fn empty_and_completed_renewal_are_terminal_without_sending() {
    let (_root, mut journal, live, mut record, mut chain) = fixture();
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::Empty
    );
    record.kind = ServiceTxKindV1::Renew;
    journal.begin(record.clone()).unwrap();
    chain.binding = Some(applied(&record));
    chain.receipt = Some(LifecycleReceiptV1 {
        tx_hash: record.tx_hash,
        block_height: chain.at.height,
        block_hash: chain.at.hash,
        success: true,
    });
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::Completed
    );
    use std::os::unix::fs::MetadataExt as _;
    let path = _root.path().join("tee-service-tx-v1/journal.json");
    let inode = std::fs::metadata(&path).unwrap().ino();
    let terminal = std::fs::read(&path).unwrap();
    chain.at.height += 1;
    chain.at.hash = B256::repeat_byte(20);
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::Completed
    );
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&path).unwrap(), terminal);
    assert_eq!(
        journal.load().unwrap().unwrap().state,
        ServiceTxStateV1::Completed
    );
}

#[tokio::test]
async fn every_canonical_binding_field_is_checked_before_nonce_rebuild() {
    type Change = fn(&mut RenewalBindingV1);
    let changes: [Change; 11] = [
        |b| b.enclave_id = B256::repeat_byte(90),
        |b| b.node_id_hash = B256::repeat_byte(90),
        |b| b.binding_id = B256::repeat_byte(90),
        |b| b.binding_version += 1,
        |b| b.registration_version += 1,
        |b| b.renewal_nonce += 1,
        |b| b.transition_nonce += 1,
        |b| b.valid_until += 1,
        |b| b.policy_hash = B256::repeat_byte(90),
        |b| b.recipient_x25519 = B256::repeat_byte(90),
        |b| b.valid_until -= 1,
    ];
    for change in changes {
        let (_root, mut journal, live, record, mut chain) = fixture();
        journal.begin(record.clone()).unwrap();
        let mut binding = applied(&record);
        change(&mut binding);
        chain.binding = Some(binding);
        chain.nonces = [5; 3];
        assert_eq!(
            reconcile_service_tx_v1(&chain, &mut journal, &live)
                .await
                .unwrap(),
            ServiceTxReconciliationV1::RebuildAndResign { nonce: 5 }
        );
        assert_eq!(
            journal.load().unwrap().unwrap().state,
            ServiceTxStateV1::RebuildRequired
        );
    }
}

#[tokio::test]
async fn receipt_or_chain_failures_preserve_the_exact_pending_journal() {
    for case in 0..6 {
        let (root, mut journal, live, record, mut chain) = fixture();
        journal.begin(record.clone()).unwrap();
        let path = root.path().join("tee-service-tx-v1/journal.json");
        let before = std::fs::read(&path).unwrap();
        let mut receipt = LifecycleReceiptV1 {
            tx_hash: record.tx_hash,
            block_height: chain.at.height,
            block_hash: chain.at.hash,
            success: true,
        };
        match case {
            0 => chain.fail_receipt = true,
            1 => receipt.tx_hash = B256::repeat_byte(90),
            2 => receipt.block_height += 1,
            3 => receipt.block_hash = B256::repeat_byte(90),
            4 => chain.at.genesis_hash = B256::repeat_byte(90),
            _ => {} // Successful receipt without matching canonical mutation fails closed.
        }
        chain.receipt = Some(receipt);
        assert!(reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn reverted_receipt_rebuilds_only_once_the_nonce_is_finalized() {
    let (_root, mut journal, live, record, mut chain) = fixture();
    journal.begin(record.clone()).unwrap();
    chain.receipt = Some(LifecycleReceiptV1 {
        tx_hash: record.tx_hash,
        block_height: chain.at.height,
        block_hash: chain.at.hash,
        success: false,
    });
    chain.nonces = [5; 3];
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::RebuildAndResign { nonce: 5 }
    );
    chain.binding = Some(applied(&record));
    assert_eq!(
        reconcile_service_tx_v1(&chain, &mut journal, &live)
            .await
            .unwrap(),
        ServiceTxReconciliationV1::IngestOnboarding
    );
}

#[tokio::test]
async fn candidate_prepare_cancel_and_transition_reconcile_distinct_mutations() {
    for kind in [
        ServiceTxKindV1::UpgradePrepare,
        ServiceTxKindV1::UpgradeCancel,
        ServiceTxKindV1::UpgradeTransition,
    ] {
        let (_root, mut journal, live, mut record, mut chain) = fixture();
        record.kind = kind;
        if kind == ServiceTxKindV1::UpgradePrepare {
            use alloy_sol_types::SolCall as _;
            use outbe_primitives::{tee_attestation_v1::*, tee_registry_abi_v1::ITeeRegistryV1};
            let manifest = live.manifest();
            let intent = RegistrationIntentV1 {
                chain_id: manifest.chain_id,
                genesis_hash: manifest.genesis_hash,
                operation: AttestationOperationV1::PrepareEnclaveUpgrade,
                attestation_mode: manifest.attestation_mode,
                policy_hash: record.public_intent.target.policy_hash,
                node_id: manifest.node_id.clone(),
                enclave_id: manifest.enclave_id().unwrap(),
                binding_id: B256::repeat_byte(55),
                binding_version: 2,
                registration_version: 2,
                renewal_nonce: 0,
                transition_nonce: 1,
                requested_valid_until: 2000,
                recipient_x25519: manifest.recipient_x25519,
                attestation_ed25519: manifest.attestation_ed25519,
                noise_responder_x25519: manifest.noise_responder_x25519,
                node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
            };
            let signature = live.sign_registration_intent_dev_v1(&intent).unwrap();
            let evidence = AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
                intent,
                dev_attestation_public: manifest.attestation_ed25519,
                dev_signature: signature,
                transition_key_ready_proof: None,
            })
            .encode_canonical()
            .unwrap();
            let calldata = ITeeRegistryV1::prepareEnclaveUpgradeCall {
                evidence: evidence.into(),
                nodeSignature: vec![0; 65].into(),
                enclaveSignature: signature.to_vec().into(),
            }
            .abi_encode();
            let mut encoded = &record.raw_signed_tx[1..];
            use alloy_consensus::transaction::RlpEcdsaDecodableTx as _;
            let mut tx = TxEip1559::rlp_decode_signed(&mut encoded)
                .unwrap()
                .tx()
                .clone();
            tx.input = calldata.into();
            let key = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
            let (sig, id) = key
                .sign_prehash_recoverable(tx.signature_hash().as_slice())
                .unwrap();
            let sig = alloy_primitives::Signature::from_signature_and_parity(sig, id.is_y_odd());
            let mut raw = vec![2];
            tx.into_signed(sig).rlp_encode(&mut raw);
            record.tx_hash = keccak256(&raw);
            record.raw_signed_tx = raw.into();
        }
        chain.binding = Some(applied(&record));
        let candidate = LifecycleUpgradeCandidateV1 {
            context_hash: B256::repeat_byte(4),
            valid_until: 2000,
            source_binding_id: B256::repeat_byte(5),
            target_hash: B256::repeat_byte(6),
            nonce: 1,
        };
        if kind != ServiceTxKindV1::UpgradeTransition {
            record.public_intent.candidate = Some(candidate);
        }
        match kind {
            ServiceTxKindV1::UpgradePrepare => chain.candidate = Some(candidate),
            ServiceTxKindV1::UpgradeCancel => chain.candidate = None,
            _ => chain.binding = Some(applied(&record)),
        }
        journal.begin(record).unwrap();
        let expected = if kind == ServiceTxKindV1::UpgradeTransition {
            ServiceTxReconciliationV1::IngestOnboarding
        } else {
            ServiceTxReconciliationV1::Completed
        };
        assert_eq!(
            reconcile_service_tx_v1(&chain, &mut journal, &live)
                .await
                .unwrap(),
            expected
        );
    }
}

#[test]
fn typed_nonce_errors_retain_the_original_source() {
    use std::error::Error as _;
    for error in [
        LifecyclePortError::NonceTooLow {
            source: std::io::Error::other("nonce detail"),
        },
        LifecyclePortError::TransactionDisplaced {
            nonce: 4,
            source: std::io::Error::other("replacement detail"),
        },
    ] {
        let source = error.source().unwrap();
        assert!(source.downcast_ref::<std::io::Error>().is_some());
    }
}
