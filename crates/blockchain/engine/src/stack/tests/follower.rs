use super::*;

#[test]
fn follower_parent_record_requires_exact_finalized_block_and_historical_committee() {
    let block = recovery_block(42);
    let round = Round::new(Epoch::new(4), View::new(9));
    let (provider, finalization) = recovery_finalization_fixture(&block, round);
    let scheme = provider
        .scoped(round.epoch())
        .expect("fixture registers the finalized epoch verifier");
    let addresses = vec![
        Address::repeat_byte(0x11),
        Address::repeat_byte(0x22),
        Address::repeat_byte(0x33),
    ];
    let encoded_pubkeys: Vec<Vec<u8>> = scheme
        .participants()
        .iter()
        .map(|public_key| public_key.encode().as_ref().to_vec())
        .collect();
    let snapshot = outbe_consensus::proof::build_committee_snapshot(
        &addresses,
        &encoded_pubkeys,
        scheme.expected_vrf_material_version(),
        scheme
            .identity()
            .map(|public_key| public_key.encode().as_ref().to_vec())
            .unwrap_or_default(),
        B256::ZERO,
    )
    .expect("fixture committee is canonical");

    let record =
        build_certified_follower_parent_record(&finalization, &block, &snapshot, scheme.as_ref())
            .expect("exact certified follower inputs build the canonical parent record");
    assert_eq!(record.finalized_block_number(), Some(block.number()));
    assert_eq!(record.finalized_block_hash, block.block_hash());
    assert_eq!(
        record.committee_set_hash,
        snapshot.committee_set_hash_v2(round.epoch().get())
    );

    let wrong_block = recovery_block(43);
    let wrong_block_error = build_certified_follower_parent_record(
        &finalization,
        &wrong_block,
        &snapshot,
        scheme.as_ref(),
    )
    .unwrap_err()
    .to_string();
    assert!(wrong_block_error.contains("finalization payload"));

    let mut wrong_snapshot = snapshot;
    wrong_snapshot.committee[0].consensus_pubkey = [0x99; 48];
    let wrong_snapshot_error = build_certified_follower_parent_record(
        &finalization,
        &block,
        &wrong_snapshot,
        scheme.as_ref(),
    )
    .unwrap_err()
    .to_string();
    assert!(wrong_snapshot_error.contains("historical committee snapshot"));
}

#[test]
fn follower_finality_observer_skips_only_the_genesis_ack() {
    assert!(!follower_height_has_certified_finalization(0));
    assert!(follower_height_has_certified_finalization(1));
    assert!(follower_height_has_certified_finalization(u64::MAX));
}

#[test]
fn follower_shutdown_keeps_certificate_available_until_observer_drains() {
    commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(17));
        let block = recovery_block(17);
        let (provider, finalization) = recovery_finalization_fixture(&block, round);
        let clock = context.child("proof_ready");
        let shutdown = context.child("shutdown");
        let (mut mailbox, _resolver, actor) = start_recovery_marshal(context, provider).await;
        let _ = mailbox.verified(round, block).await;
        let _ = mailbox.report(Activity::Finalization(finalization));
        recover_application_finalized_round(clock, mailbox.clone(), 17)
            .await
            .unwrap();
        // Use the NodeHost pre-stop handshake. An accepted notification must
        // still be able to obtain its exact proof after a shutdown request.
        let (control, drain) = crate::follower_shutdown::follower_drain_pair();
        let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
        let _reporter = drain
            .install(outbe_consensus::executor::Mailbox::from_sender(tx))
            .unwrap()
            .unwrap();
        let waiting = control.drain(Duration::from_secs(5));
        tokio::pin!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());
        let observer = async {
            use futures::StreamExt as _;
            assert!(
                deliveries.next().await.is_none(),
                "ingress must close before proof drain"
            );
            assert!(
                mailbox.get_finalization(Height::new(17)).await.is_some(),
                "marshal has no certified finalization at follower height 17 during shutdown drain"
            );
            Ok(())
        };
        drain.finish(async { Ok(()) }, observer).await.unwrap();
        waiting.await.unwrap();
        shutdown
            .stop(0, Some(Duration::from_secs(5)))
            .await
            .unwrap();
        actor.await.unwrap();
        assert!(
            mailbox.get_finalization(Height::new(17)).await.is_none(),
            "only after proof drain may Marshal be unavailable"
        );
    });
}

#[test]
fn follower_shutdown_drains_real_delivery_and_reopens_exact_persisted_proof() {
    use futures::StreamExt as _;
    use outbe_consensus::finalization::parent_cert_store::{
        CertifiedParentProofStore as _, FinalizedParentCertStore,
    };

    let directory = tempfile::tempdir().unwrap();
    let proof_dir = directory.path().join("proofs");
    let result = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(17));
        let mut raw = Block::default();
        raw.header.number = 1;
        raw.header.parent_hash = recovery_block(0).block_hash();
        let block =
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw.map_header(OutbeHeader::new)));
        let (provider, finalization) = recovery_finalization_fixture(&block, round);
        let scheme = provider.scoped(round.epoch()).unwrap();
        let addresses = vec![
            Address::repeat_byte(1),
            Address::repeat_byte(2),
            Address::repeat_byte(3),
        ];
        let public_keys: Vec<Vec<u8>> = scheme
            .participants()
            .iter()
            .map(|key| key.encode().as_ref().to_vec())
            .collect();
        let snapshot = outbe_consensus::proof::build_committee_snapshot(
            &addresses,
            &public_keys,
            scheme.expected_vrf_material_version(),
            scheme
                .identity()
                .map(|key| key.encode().as_ref().to_vec())
                .unwrap_or_default(),
            B256::ZERO,
        )
        .unwrap();
        let (control, drain) = crate::follower_shutdown::follower_drain_pair();
        let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
        let reporter = drain
            .install(outbe_consensus::executor::Mailbox::from_sender(tx))
            .unwrap()
            .unwrap();
        let shutdown = context.child("shutdown");
        let (mut mailbox, _resolver, actor) =
            start_recovery_marshal_with_reporter(context, provider, reporter).await;
        let _ = mailbox.verified(round, block.clone()).await;
        let _ = mailbox.report(Activity::Finalization(finalization));
        let accepted_ack = loop {
            let message = tokio::time::timeout(Duration::from_secs(5), deliveries.next())
                .await
                .unwrap()
                .unwrap();
            if let outbe_consensus::executor::ingress::Message::MarshalUpdate(update) = message {
                if let Update::Block(delivered, ack) = *update {
                    if delivered.number() == 0 {
                        ack.acknowledge();
                        continue;
                    }
                    assert_eq!(delivered.block_hash(), block.block_hash());
                    break ack;
                }
            }
        };
        // Freeze the accepted delivery before its executor ACK, then request
        // shutdown. This is the exact race cut from lifecycle scenarios4/9.
        let wait = control.drain(Duration::from_secs(5));
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        assert!(deliveries.next().await.is_none());
        let store = FinalizedParentCertStore::open(&proof_dir).unwrap();
        let cert = mailbox.get_finalization(Height::new(1)).await.unwrap();
        let record =
            build_certified_follower_parent_record(&cert, &block, &snapshot, &scheme).unwrap();
        let key = record.proof_key();
        let expected = record.clone();
        let execution = async {
            accepted_ack.acknowledge();
            Ok(())
        };
        let observer = async {
            // Still available AFTER stop request, through the real mailbox.
            let exact = mailbox.get_finalization(Height::new(1)).await.unwrap();
            assert_eq!(exact.encode(), cert.encode());
            store.put_finalization(record).map_err(eyre::Report::new)
        };
        drain.finish(execution, observer).await.unwrap();
        wait.await.unwrap();
        assert_eq!(mailbox.get_processed_height().await, Some(Height::new(1)));
        shutdown
            .stop(0, Some(Duration::from_secs(5)))
            .await
            .unwrap();
        actor.await.unwrap();
        drop(store);
        let reopened = FinalizedParentCertStore::open(&proof_dir).unwrap();
        assert_eq!(reopened.get_finalization(key), Some(expected));
        true
    });
    assert!(result);
}

#[test]
fn publish_randomness_status_preserves_last_finalized_block() {
    use outbe_primitives::consensus::RandomnessStatus;

    let bridge = ConsensusExecutionBridge::new();
    bridge.set_last_finalized_block_number(77);

    let vrf_safety = VrfSafetyGate::new(3, 100, 300, 40);
    vrf_safety.note_preparing(2, 260, 300, 40);

    publish_randomness_status(&bridge, &vrf_safety);

    let status = bridge.consensus_status();
    assert_eq!(status.last_finalized_block, 77);
    assert_eq!(status.randomness_status, RandomnessStatus::Preparing);
    assert_eq!(status.vrf_material_version, 3);
    assert_eq!(status.last_dkg_activation_height, 100);
    assert_eq!(status.next_planned_activation_height, 300);
    assert_eq!(status.vrf_expiry_height, 340);
}

#[test]
fn finalization_drainer_skips_closed_replies_and_answers_live_requests() {
    use outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore;
    use std::task::{Context, Poll, Waker};

    let directory = tempfile::tempdir().unwrap();
    let proof_dir = directory.path().join("proofs");
    commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(17));
        let block = recovery_block(17);
        let (provider, finalization) = recovery_finalization_fixture(&block, round);
        let clock = context.child("proof_ready");
        let drainer_ctx = context.child("drainer_parent");
        let shutdown = context.child("shutdown");
        let (mut mailbox, _resolver, actor) = start_recovery_marshal(context, provider).await;
        let _ = mailbox.verified(round, block.clone()).await;
        let _ = mailbox.report(Activity::Finalization(finalization.clone()));
        recover_application_finalized_round(clock, mailbox.clone(), 17)
            .await
            .unwrap();

        let bridge = ConsensusExecutionBridge::new();
        let store = FinalizedParentCertStore::open(&proof_dir).unwrap();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let drainer =
            spawn_finalization_drainer(&drainer_ctx, mailbox, bridge.clone(), store, cancel_rx);

        // Enqueue a request and immediately drop the caller future so `reply.is_closed()` is true.
        let mut cx = Context::from_waker(Waker::noop());
        {
            let b = bridge.clone();
            let mut cancelled_fut = Box::pin(async move { b.request_finalization(999).await });
            assert!(matches!(
                cancelled_fut.as_mut().poll(&mut cx),
                Poll::Pending
            ));
        }

        // Next live request for height 17 must succeed with exact encoded bytes.
        let live = bridge
            .request_finalization(17)
            .await
            .expect("drainer is serving")
            .expect("height 17 is finalized in marshal");
        assert_eq!(live.finalization.as_ref(), finalization.encode().as_ref());
        assert_eq!(live.block.as_ref(), block.encode().as_ref());

        // Missing height returns Ok(None).
        let missing = bridge
            .request_finalization(18)
            .await
            .expect("drainer is serving");
        assert!(missing.is_none());

        let _ = cancel_tx.send(true);
        drainer.await.unwrap();
        shutdown
            .stop(0, Some(Duration::from_secs(5)))
            .await
            .unwrap();
        actor.await.unwrap();
    });
}
