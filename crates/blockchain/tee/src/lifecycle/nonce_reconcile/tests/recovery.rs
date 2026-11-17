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
