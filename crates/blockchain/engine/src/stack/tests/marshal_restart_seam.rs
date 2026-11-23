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
