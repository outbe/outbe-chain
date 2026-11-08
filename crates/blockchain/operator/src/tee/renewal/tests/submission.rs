use super::super::*;
use super::replay::{renewal_services, replay_fixture};

fn store_prepared(dir: &std::path::Path, attempt: PreparedRenewalV1) {
    RenewalJournalGuard::acquire(dir)
        .unwrap()
        .store(RenewalJournalSnapshotV1::new(
            RenewalJournalStateV1::Prepared { attempt },
        ))
        .unwrap();
}

fn journal_label(dir: &std::path::Path) -> String {
    RenewalJournalGuard::acquire(dir)
        .unwrap()
        .load()
        .unwrap()
        .unwrap()
        .lifecycle
        .label()
        .to_owned()
}

#[tokio::test]
async fn already_known_without_exact_receipt_preserves_the_pending_attempt() {
    for starts_submitted in [false, true] {
        let node_data_dir = tempfile::tempdir().unwrap();
        let (mut rpc, relay, config, attempt) =
            replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
        let expected_hash = attempt.relay_variants[0].transaction_hash;
        let expected_raw = attempt.relay_variants[0].raw_transaction.clone();
        let lifecycle = if starts_submitted {
            RenewalJournalStateV1::Submitted {
                attempt: attempt.clone(),
                submitted_at_finalized_height: 119,
                transaction_hashes: vec![expected_hash],
            }
        } else {
            RenewalJournalStateV1::Prepared {
                attempt: attempt.clone(),
            }
        };
        RenewalJournalGuard::acquire(node_data_dir.path())
            .unwrap()
            .store(RenewalJournalSnapshotV1::new(lifecycle.clone()))
            .unwrap();
        rpc.send_error = Some("already known");
        let mut enclave = super::replay::ReplayMustNotPrepare;
        let signer =
            |_hash: B256| -> Result<[u8; 65]> { panic!("pending renewal must not re-sign") };

        for _ in 0..2 {
            let error = run_renewal_once_v1(
                renewal_services(&rpc, &relay, &mut enclave, &signer),
                &config,
            )
            .await
            .expect_err("endpoint text alone must not submit a renewal");
            assert!(matches!(
                error.downcast_ref::<RenewalError>(),
                Some(RenewalError::AwaitingExactReceipt { transaction_hash })
                    if *transaction_hash == expected_hash
            ));
            let snapshot = RenewalJournalGuard::acquire(node_data_dir.path())
                .unwrap()
                .load()
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.lifecycle, lifecycle);
        }
        assert_eq!(
            rpc.sent.lock().unwrap().as_slice(),
            [expected_raw.clone(), expected_raw]
        );

        *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
            "transactionHash": format!("{expected_hash:#x}"),
            "status": "0x1",
        }));
        let outcome = run_renewal_once_v1(
            renewal_services(&rpc, &relay, &mut enclave, &signer),
            &config,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RenewalOutcomeV1::Submitted {
                transaction_hash: expected_hash,
                replayed: true,
            }
        );
        assert_eq!(rpc.sent.lock().unwrap().len(), 2);
        let snapshot = RenewalJournalGuard::acquire(node_data_dir.path())
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        let RenewalJournalStateV1::Submitted {
            attempt: persisted_attempt,
            transaction_hashes,
            ..
        } = snapshot.lifecycle
        else {
            panic!("an exact receipt must persist Submitted without claiming finality");
        };
        assert_eq!(persisted_attempt, attempt);
        assert_eq!(transaction_hashes, vec![expected_hash]);
    }
}

#[tokio::test]
async fn already_known_with_failed_receipt_keeps_the_prepared_journal() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (mut rpc, relay, config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let expected_hash = attempt.relay_variants[0].transaction_hash;
    rpc.send_error = Some("already known");
    *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
        "transactionHash": format!("{expected_hash:#x}"),
        "status": "0x0",
    }));
    store_prepared(node_data_dir.path(), attempt);
    let mut enclave = super::replay::ReplayMustNotPrepare;
    let signer = |_hash: B256| -> Result<[u8; 65]> { panic!("failed receipt must not re-sign") };
    let error = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &signer),
        &config,
    )
    .await
    .expect_err("a failed receipt must not submit a renewal");
    assert!(
        error.to_string().contains("non-success status"),
        "{error:#}"
    );
    assert_eq!(journal_label(node_data_dir.path()), "prepared");
}

#[tokio::test]
async fn already_known_with_exact_success_receipt_submits() {
    let node_data_dir = tempfile::tempdir().unwrap();
    let (mut rpc, relay, config, attempt) =
        replay_fixture(node_data_dir.path(), AttestationMode::GramineDirectDev);
    let expected_hash = attempt.relay_variants[0].transaction_hash;
    rpc.send_error = Some("already known");
    *rpc.receipt.lock().unwrap() = Some(serde_json::json!({
        "transactionHash": format!("{expected_hash:#x}"),
        "status": "0x1",
    }));
    store_prepared(node_data_dir.path(), attempt);
    let mut enclave = super::replay::ReplayMustNotPrepare;
    let signer =
        |_hash: B256| -> Result<[u8; 65]> { panic!("exact success receipt must not re-sign") };
    let outcome = run_renewal_once_v1(
        renewal_services(&rpc, &relay, &mut enclave, &signer),
        &config,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        RenewalOutcomeV1::Submitted {
            transaction_hash: expected_hash,
            replayed: true,
        }
    );
    assert_eq!(journal_label(node_data_dir.path()), "submitted");
}
