//! Multi-height (`H1 -> H2`) disk-backed Marshal restart seam tests.
//!
//! Uses separate `commonware_tokio::Runner` instances over a shared on-disk storage directory and
//! Outbe's production `start_recovery_marshal_in_partition` opener to verify `Processed::Block`
//! recovery, finalized-round persistence, and strict absence of redelivery across restarts at both
//! `H1` and `H2`.

use super::*;
use commonware_consensus::marshal::core::Processed;
use futures::StreamExt as _;

const MAX_POLL_ATTEMPTS: usize = 2_000;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
const PARTITION_PREFIX: &str = "marshal-restart-seam-h1-h2";

async fn wait_for_processed(
    context: &commonware_tokio::Context,
    mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
    expected: Processed,
) {
    for _ in 0..MAX_POLL_ATTEMPTS {
        if mailbox.get_processed().await == Some(expected) {
            return;
        }
        context.sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "timed out waiting for Processed::{expected:?}, last observed: {:?}",
        mailbox.get_processed().await
    );
}

async fn ack_deliveries_through(
    deliveries: &mut futures::channel::mpsc::UnboundedReceiver<
        outbe_consensus::executor::ingress::Message,
    >,
    target_height: u64,
) -> Vec<u64> {
    let mut acked = Vec::new();
    while acked.last().copied() != Some(target_height) {
        let msg = tokio::time::timeout(DELIVERY_TIMEOUT, deliveries.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for marshal delivery up to height {target_height}; acked so far: {acked:?}"
                )
            })
            .expect("marshal delivery");
        if let outbe_consensus::executor::ingress::Message::MarshalUpdate(update) = msg {
            if let Update::Block(delivered, ack) = *update {
                acked.push(delivered.inner().number());
                ack.acknowledge();
            }
        }
    }
    acked
}

async fn assert_no_block_redelivery(
    context: &commonware_tokio::Context,
    deliveries: &mut futures::channel::mpsc::UnboundedReceiver<
        outbe_consensus::executor::ingress::Message,
    >,
    context_label: &str,
) {
    context.sleep(Duration::from_millis(50)).await;
    while let Ok(msg) = deliveries.try_recv() {
        if let outbe_consensus::executor::ingress::Message::MarshalUpdate(update) = msg {
            if let Update::Block(delivered, _) = *update {
                panic!(
                    "{context_label}: unexpected redelivery of already-processed block height {}",
                    delivered.inner().number()
                );
            }
        }
    }
}

#[test]
fn marshal_restart_recovers_processed_blocks_and_rounds_across_h1_to_h2() {
    let storage_dir = tempfile::tempdir().expect("tempdir for marshal restart seam");

    let keys: Vec<bls12381::PrivateKey> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let dkg = outbe_consensus::bls::bootstrap_dkg(3).unwrap();
    let signers = bootstrap_fixture_signers(&keys, &participants, &dkg);
    let verifier = HybridScheme::<MinSig>::verifier(
        &config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .unwrap();

    let genesis = recovery_block(0);
    let mut raw_h1 = Block::default();
    raw_h1.header.number = 1;
    raw_h1.header.parent_hash = genesis.block_hash();
    let h1 =
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw_h1.map_header(OutbeHeader::new)));
    let mut raw_h2 = Block::default();
    raw_h2.header.number = 2;
    raw_h2.header.parent_hash = h1.block_hash();
    let h2 =
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw_h2.map_header(OutbeHeader::new)));

    let r1 = Round::new(Epoch::new(0), View::new(1));
    let r2 = Round::new(Epoch::new(0), View::new(2));
    let f1 = certify_fixture_proposal(
        &verifier,
        &signers,
        Proposal::new(r1, View::zero(), h1.digest()),
    );
    let f2 = certify_fixture_proposal(
        &verifier,
        &signers,
        Proposal::new(r2, View::new(1), h2.digest()),
    );

    let runner_cfg = || {
        commonware_tokio::Config::default()
            .with_worker_threads(2)
            .with_storage_directory(storage_dir.path().to_path_buf())
    };

    // Boot 1: finalize and acknowledge H1 on persistent disk storage, then shut down.
    {
        let verifier = verifier.clone();
        let genesis = genesis.clone();
        let h1 = h1.clone();
        let f1 = f1.clone();
        commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier));
            let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
            let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
            let (mut mailbox, handler_keepalive, handle) =
                outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                    context.child("boot1"),
                    provider,
                    reporter,
                    PARTITION_PREFIX.to_string(),
                    genesis,
                )
                .await;

            assert!(
                mailbox
                    .verified(
                        r1,
                        commonware_cryptography::Digested::precomputed(h1.clone(), h1.digest()),
                    )
                    .await
            );
            let _ = mailbox.report(Activity::Finalization(f1));
            assert_eq!(ack_deliveries_through(&mut deliveries, 1).await, vec![0, 1]);
            wait_for_processed(&context, &mailbox, Processed::Block(Height::new(1))).await;

            handle.abort();
            let _ = handle.await;
            drop(handler_keepalive);
        });
    }

    // Boot 2 (fresh Runner after H1 restart): recover H1 without redelivery, then advance to H2.
    {
        let verifier = verifier.clone();
        let genesis = genesis.clone();
        let h1 = h1.clone();
        let h2 = h2.clone();
        let f2 = f2.clone();
        commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier));
            let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
            let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
            let (mut mailbox, handler_keepalive, handle) =
                outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                    context.child("boot2"),
                    provider,
                    reporter,
                    PARTITION_PREFIX.to_string(),
                    genesis,
                )
                .await;

            assert_eq!(
                mailbox.get_processed().await,
                Some(Processed::Block(Height::new(1))),
                "boot 2 must recover Processed::Block(1) from disk"
            );
            let rec_h1 =
                recover_application_finalized_round(context.child("rec_h1"), mailbox.clone(), 1)
                    .await
                    .unwrap()
                    .expect("H1 round must survive first restart");
            assert_eq!(rec_h1.round, r1);
            assert_eq!(rec_h1.digest, h1.digest());
            assert_no_block_redelivery(&context, &mut deliveries, "after H1 restart").await;

            assert!(
                mailbox
                    .verified(
                        r2,
                        commonware_cryptography::Digested::precomputed(h2.clone(), h2.digest()),
                    )
                    .await
            );
            let _ = mailbox.report(Activity::Finalization(f2));
            assert_eq!(ack_deliveries_through(&mut deliveries, 2).await, vec![2]);
            wait_for_processed(&context, &mailbox, Processed::Block(Height::new(2))).await;

            handle.abort();
            let _ = handle.await;
            drop(handler_keepalive);
        });
    }

    // Boot 3 (fresh Runner after H2 restart): recover H1 & H2 rounds with zero redelivery.
    {
        commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier));
            let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
            let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
            let (mailbox, handler_keepalive, handle) =
                outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                    context.child("boot3"),
                    provider,
                    reporter,
                    PARTITION_PREFIX.to_string(),
                    genesis,
                )
                .await;

            assert_eq!(
                mailbox.get_processed().await,
                Some(Processed::Block(Height::new(2))),
                "boot 3 must recover Processed::Block(2) from disk"
            );
            assert_no_block_redelivery(&context, &mut deliveries, "after H2 restart").await;

            let rec_h1 =
                recover_application_finalized_round(context.child("rec_h1"), mailbox.clone(), 1)
                    .await
                    .unwrap()
                    .expect("H1 round must survive second restart");
            let rec_h2 =
                recover_application_finalized_round(context.child("rec_h2"), mailbox.clone(), 2)
                    .await
                    .unwrap()
                    .expect("H2 round must survive second restart");
            assert_eq!(rec_h1.round, r1);
            assert_eq!(rec_h1.digest, h1.digest());
            assert_eq!(rec_h2.round, r2);
            assert_eq!(rec_h2.digest, h2.digest());
            assert!(mailbox.get_finalization(Height::new(1)).await.is_some());
            assert!(mailbox.get_finalization(Height::new(2)).await.is_some());

            handle.abort();
            let _ = handle.await;
            drop(handler_keepalive);
        });
    }
}

#[test]
fn marshal_sparse_ancestor_close_reopen_recovers_own_round_for_f_and_cn_child_witnesses() {
    use outbe_consensus::finalization::actor::FinalizationActorBuilder as _;
    use outbe_consensus::finalization::parent_cert_store::{
        CertifiedParentProofKey, CertifiedParentProofStore as _, FinalizedParentCertStore,
        ProofKind,
    };
    use outbe_primitives::consensus_metadata::ParentParticipationProof;

    for (idx, proof_kind) in [
        ParentParticipationProof::Finalization,
        ParentParticipationProof::CertifiedNotarization,
    ]
    .into_iter()
    .enumerate()
    {
        let storage_dir = tempfile::tempdir().expect("tempdir for sparse ancestor seam");
        let cert_store_dir = tempfile::tempdir().expect("tempdir for parent cert store");
        let partition = format!("marshal-sparse-ancestor-seam-{idx}");
        let committee = AuthenticatedRecoveryCommittee::new(0, 100 + idx as u64 * 10, 0, 1);

        let genesis = recovery_block(0);
        let h1 = AuthenticatedRecoveryCommittee::block_with_extra_data(
            1,
            genesis.digest().0,
            committee.boundary_extra_data.clone(),
        );
        let h2 =
            AuthenticatedRecoveryCommittee::block_with_extra_data(2, h1.digest().0, Bytes::new());
        // Sparse ancestor H = 2 has Round_H = (0, 2); child H+1 = 3 carries the witness for H = 2.
        let h3 = committee.block_with_parent_witness(&h2, 2, 1, proof_kind, Bytes::new());
        let h4 = committee.block_with_parent_witness(&h3, 3, 2, proof_kind, Bytes::new());

        let r1 = Round::new(Epoch::new(0), View::new(1));
        let r2 = Round::new(Epoch::new(0), View::new(2));
        let r3 = Round::new(Epoch::new(0), View::new(3));
        let r4 = Round::new(Epoch::new(0), View::new(4));
        let f1 = committee.sign_finalization(1, 0, h1.digest());
        // Report F(4) only (no direct F(2) or F(3)) so H = 2 is a genuine sparse ancestor.
        let f4 = committee.sign_finalization(4, 3, h4.digest());

        let mut headers = BTreeMap::new();
        headers.insert(1, AuthenticatedRecoveryCommittee::header_record(&h1));
        headers.insert(2, AuthenticatedRecoveryCommittee::header_record(&h2));
        let mut snapshots = BTreeMap::new();
        snapshots.insert((h2.digest().0, 0), committee.snapshot.clone());
        let history = StaticCanonicalHistory {
            genesis_participants: committee.participants.clone(),
            headers,
            snapshots,
        };
        let committee_cfg = RecoveryCommitteeConfig {
            epoch_length_blocks: 24,
            prepare_window_blocks: 8,
            activation_grace_blocks: 60,
        };

        let runner_cfg = || {
            commonware_tokio::Config::default()
                .with_worker_threads(2)
                .with_storage_directory(storage_dir.path().to_path_buf())
        };

        // Boot 1: persist blocks 1..=4 and F(1), F(4), acknowledge deliveries only through H=2.
        {
            let verifier = committee.verifier.clone();
            let genesis = genesis.clone();
            let (h1, h2, h3, h4) = (h1.clone(), h2.clone(), h3.clone(), h4.clone());
            let (f1, f4) = (f1.clone(), f4.clone());
            let partition = partition.clone();
            commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
                let provider = HybridSchemeProvider::new();
                assert!(provider.register(Epoch::new(0), verifier));
                let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
                let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
                let (mut mailbox, handler_keepalive, handle) =
                    outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                        context.child("boot1"),
                        provider,
                        reporter,
                        partition,
                        genesis,
                    )
                    .await;

                for (round, blk) in [(r1, h1), (r2, h2), (r3, h3), (r4, h4)] {
                    assert!(
                        mailbox
                            .verified(
                                round,
                                commonware_cryptography::Digested::precomputed(
                                    blk.clone(),
                                    blk.digest(),
                                ),
                            )
                            .await
                    );
                }
                let _ = mailbox.report(Activity::Finalization(f1));
                let _ = mailbox.report(Activity::Finalization(f4));
                assert_eq!(
                    ack_deliveries_through(&mut deliveries, 2).await,
                    vec![0, 1, 2]
                );
                wait_for_processed(&context, &mailbox, Processed::Block(Height::new(2))).await;

                handle.abort();
                let _ = handle.await;
                drop(handler_keepalive);
            });
        }

        // Boot 2: fresh Runner reopens Marshal at Processed::Block(2) with no F(2), authenticates H=2 from F(4).
        {
            let verifier = committee.verifier.clone();
            let ordered_addresses: Vec<Address> = committee
                .snapshot
                .committee
                .iter()
                .map(|entry| entry.address)
                .collect();
            let genesis = genesis.clone();
            let h2 = h2.clone();
            let h4 = h4.clone();
            let history = history.clone();
            let cert_store_path = cert_store_dir.path().to_path_buf();
            commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
                let provider = HybridSchemeProvider::new();
                assert!(provider.register(Epoch::new(0), verifier.clone()));
                let (tx, mut deliveries) = futures::channel::mpsc::unbounded();
                let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
                let (mailbox, handler_keepalive, handle) =
                    outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                        context.child("boot2"),
                        provider,
                        reporter,
                        partition,
                        genesis,
                    )
                    .await;

                assert_eq!(
                    mailbox.get_processed().await,
                    Some(Processed::Block(Height::new(2)))
                );
                assert!(
                    mailbox.get_finalization(Height::new(2)).await.is_none(),
                    "H=2 must be a sparse ancestor without a direct F(2)"
                );
                assert!(
                    mailbox.get_finalization(Height::new(4)).await.is_some(),
                    "descendant F(4) must survive close/reopen"
                );

                let recovered = recover_authenticated_application_finalization(
                    context.child("recover_sparse"),
                    mailbox.clone(),
                    2,
                    h2.digest().0,
                    4,
                    &history,
                    committee_cfg,
                )
                .await
                .expect("sparse ancestor recovery must succeed")
                .expect("non-genesis recovery must return AuthenticatedApplicationFinalization");
                let (anchor_h, anchor_hash, anchor_round, record_opt) =
                    reconcile_authenticated_execution_head(2, h2.digest().0, Some(recovered))
                        .expect("reconcile authenticated execution head");

                assert_eq!(anchor_h, 2);
                assert_eq!(anchor_hash, h2.digest().0);
                assert_eq!(
                    anchor_round,
                    Some(r2),
                    "must recover exact own Round_H=(0,2), never descendant Round_K=(0,4)"
                );
                assert_ne!(anchor_round, Some(r4));

                let record = record_opt.expect("actual-kind parent proof record");
                let store = FinalizedParentCertStore::open(&cert_store_path).unwrap();
                let key_h2 = CertifiedParentProofKey::new(0, 2, h2.digest().0);
                match proof_kind {
                    ParentParticipationProof::Finalization => {
                        assert_eq!(
                            record.kind,
                            ProofKind::Finalization {
                                finalized_block_number: 2
                            }
                        );
                        store.put_finalization(record).unwrap();
                        assert!(store.get_finalization(key_h2).is_some());
                        assert!(store.get_certified_notarization(key_h2).is_none());
                    }
                    ParentParticipationProof::CertifiedNotarization => {
                        assert_eq!(record.kind, ProofKind::CertifiedNotarization);
                        store.put_certified_notarization(record).unwrap();
                        assert!(store.get_certified_notarization(key_h2).is_some());
                        assert!(store.get_finalization(key_h2).is_none());
                    }
                }

                // T1 (INV-9): seed FinalizationView + FinalizationActor exactly as production startup does,
                // drive reconciliation of F(4), and prove FinalizationView advances to (4, hash_4, Some(r4))
                // without SameRoundInconsistency while retaining H=2's actual-kind record and storing F(4).
                let view_handle = new_finalization_view(anchor_hash, anchor_h, anchor_round);
                let actor_schemes = HybridSchemeProvider::new();
                assert!(actor_schemes.register(Epoch::new(0), verifier));
                let actor_committees = CommitteeProvider::new();
                actor_committees.register(Epoch::new(0), ordered_addresses);
                let (finalization_actor, finalization_mailbox) =
                    outbe_consensus::finalization::actor::FinalizationActor::new(
                        outbe_consensus::finalization::actor::FinalizationActorDeps {
                            view: view_handle.clone(),
                            block_cache:
                                outbe_consensus::finalization::block_cache::BlockCache::new(),
                            marshal_mailbox: Some(mailbox.clone()),
                            bridge: None,
                            dkg_manager: DkgManagerMailbox::new(),
                            vrf_safety: VrfSafetyGate::new(0, 0, 24, 60),
                            parent_cert_store: store.clone(),
                            certificate_scheme_provider: actor_schemes,
                            committee_provider: actor_committees,
                            late_sig_store: outbe_consensus::finalization::late_sig_store::shared(
                                outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
                            ),
                        },
                    )
                    .with_epocher(
                        outbe_consensus::follow::FollowerEpocher::from_anchor(
                            24,
                            60,
                            Epoch::new(0),
                            Height::new(1),
                        ),
                    );
                let finalization_handle = context
                    .child("finalization_actor")
                    .spawn(move |ctx| finalization_actor.run(ctx));

                finalization_mailbox
                    .reconcile_marshal_tip(outbe_consensus::marshal_update_reporter::ConsensusTip {
                        round: r4,
                        height: Height::new(4),
                        digest: h4.digest(),
                    })
                    .expect("enqueue F(4) reconciliation");

                for _ in 0..MAX_POLL_ATTEMPTS {
                    if view_handle.read().last_finalized_number == 4 {
                        break;
                    }
                    context.sleep(Duration::from_millis(5)).await;
                }
                {
                    let view = view_handle.read();
                    assert_eq!(
                        view.last_finalized_number, 4,
                        "FinalizationActor must advance view from recovered H=2 to K=4"
                    );
                    assert_eq!(view.forkchoice.finalized_block_hash, h4.digest().0);
                    assert_eq!(view.last_finalized_round, Some(r4));
                }

                // H=2 retains its exact actual-kind record and K=4 has its persisted Finalization record.
                let key_h4 = CertifiedParentProofKey::new(0, 4, h4.digest().0);
                assert!(
                    store.get_finalization(key_h4).is_some(),
                    "FinalizationActor must persist F(4) parent-proof record"
                );
                match proof_kind {
                    ParentParticipationProof::Finalization => {
                        assert!(store.get_finalization(key_h2).is_some());
                        assert!(store.get_certified_notarization(key_h2).is_none());
                    }
                    ParentParticipationProof::CertifiedNotarization => {
                        assert!(store.get_certified_notarization(key_h2).is_some());
                        assert!(store.get_finalization(key_h2).is_none());
                    }
                }

                drop(finalization_mailbox);
                finalization_handle.abort();
                let _ = finalization_handle.await;

                // Only blocks > 2 (3 and 4) may be delivered after reopen; never <= 2.
                let delivered_after_reopen = ack_deliveries_through(&mut deliveries, 4).await;
                assert_eq!(delivered_after_reopen, vec![3, 4]);
                assert_no_block_redelivery(&context, &mut deliveries, "after sparse reopen").await;

                handle.abort();
                let _ = handle.await;
                drop(handler_keepalive);
            });
        }
    }
}

#[test]
fn marshal_sparse_ancestor_beyond_rpc_64_and_pruned_below_h_recovers() {
    use outbe_consensus::finalization::parent_cert_store::ProofKind;
    use outbe_primitives::consensus_metadata::ParentParticipationProof;

    let storage_dir = tempfile::tempdir().expect("tempdir for long sparse chain");
    let partition = "marshal-sparse-beyond-64-pruned".to_string();
    let committee = AuthenticatedRecoveryCommittee::new(0, 220, 0, 1);

    let genesis = recovery_block(0);
    let mut blocks: Vec<ConsensusBlock> = Vec::with_capacity(75);
    blocks.push(genesis.clone());
    let b1 = AuthenticatedRecoveryCommittee::block_with_extra_data(
        1,
        genesis.digest().0,
        committee.boundary_extra_data.clone(),
    );
    blocks.push(b1);
    for h in 2u64..=5 {
        let prev = blocks.last().unwrap();
        blocks.push(AuthenticatedRecoveryCommittee::block_with_extra_data(
            h,
            prev.digest().0,
            Bytes::new(),
        ));
    }
    // H = 5, K = 74 => K - H = 69 > 64 (exceeds external RPC 64-ancestor envelope cap).
    for h in 6u64..=74 {
        let prev = blocks.last().unwrap().clone();
        blocks.push(committee.block_with_parent_witness(
            &prev,
            h - 1,
            h - 2,
            ParentParticipationProof::CertifiedNotarization,
            Bytes::new(),
        ));
    }
    let f74 = committee.sign_finalization(74, 73, blocks[74].digest());

    // Canonical headers 1..=5 live in CanonicalHistorySource; Marshal will ONLY store blocks 5..=74 + F(74) (pruned below H=5).
    let mut headers = BTreeMap::new();
    for blk in &blocks[1..=5] {
        headers.insert(
            blk.number(),
            AuthenticatedRecoveryCommittee::header_record(blk),
        );
    }
    let mut snapshots = BTreeMap::new();
    snapshots.insert((blocks[5].digest().0, 0), committee.snapshot.clone());
    let history = StaticCanonicalHistory {
        genesis_participants: committee.participants.clone(),
        headers,
        snapshots,
    };
    let committee_cfg = RecoveryCommitteeConfig {
        epoch_length_blocks: 128,
        prepare_window_blocks: 8,
        activation_grace_blocks: 60,
    };

    let runner_cfg = || {
        commonware_tokio::Config::default()
            .with_worker_threads(2)
            .with_storage_directory(storage_dir.path().to_path_buf())
    };

    // Boot 1: store ONLY blocks 5..=74 and F(74) in Marshal (zero blocks/finalizations below H=5).
    {
        let verifier = committee.verifier.clone();
        let genesis = genesis.clone();
        let tail_blocks: Vec<ConsensusBlock> = blocks[5..=74].to_vec();
        let f74 = f74.clone();
        let partition = partition.clone();
        commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier));
            let (tx, _deliveries) = futures::channel::mpsc::unbounded();
            let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
            let (mut mailbox, handler_keepalive, handle) =
                outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                    context.child("boot1"),
                    provider,
                    reporter,
                    partition,
                    genesis,
                )
                .await;

            for blk in &tail_blocks {
                let r = Round::new(Epoch::new(0), View::new(blk.number()));
                assert!(
                    mailbox
                        .verified(
                            r,
                            commonware_cryptography::Digested::precomputed(
                                blk.clone(),
                                blk.digest(),
                            ),
                        )
                        .await
                );
            }
            let _ = mailbox.report(Activity::Finalization(f74));
            // Wait until F(74) is durably visible in Marshal before closing.
            for _ in 0..MAX_POLL_ATTEMPTS {
                if mailbox.get_finalization(Height::new(74)).await.is_some() {
                    break;
                }
                context.sleep(Duration::from_millis(5)).await;
            }
            assert!(mailbox.get_finalization(Height::new(74)).await.is_some());

            handle.abort();
            let _ = handle.await;
            drop(handler_keepalive);
        });
    }

    // Boot 2: fresh Runner recovers H=5 across 69 sparse blocks with zero Marshal reads below H=5.
    {
        let verifier = committee.verifier.clone();
        let genesis = genesis.clone();
        let h5_hash = blocks[5].digest().0;
        commonware_tokio::Runner::new(runner_cfg()).start(|context| async move {
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier));
            let (tx, _deliveries) = futures::channel::mpsc::unbounded();
            let reporter = outbe_consensus::executor::Mailbox::from_sender(tx);
            let (mailbox, handler_keepalive, handle) =
                outbe_consensus::stack::recovery_marshal::start_recovery_marshal_in_partition(
                    context.child("boot2"),
                    provider,
                    reporter,
                    partition,
                    genesis,
                )
                .await;

            assert!(
                mailbox.get_block(Height::new(1)).await.is_none(),
                "Marshal archive must have no blocks stored below H=5"
            );
            assert!(mailbox.get_finalization(Height::new(5)).await.is_none());

            let recovered = recover_authenticated_application_finalization(
                context.child("recover_long_sparse"),
                mailbox,
                5,
                h5_hash,
                74,
                &history,
                committee_cfg,
            )
            .await
            .expect("69-block sparse ancestor beyond 64 must recover cleanly")
            .expect("non-genesis recovery");
            assert_eq!(recovered.round, Round::new(Epoch::new(0), View::new(5)));
            assert_eq!(recovered.digest.0, h5_hash);
            assert_eq!(
                recovered.parent_proof_record.kind,
                ProofKind::CertifiedNotarization
            );

            handle.abort();
            let _ = handle.await;
            drop(handler_keepalive);
        });
    }
}
