use super::*;
use crate::application::actor::OutbeApplication;
use commonware_consensus::CertifiableAutomaton as _;
use commonware_runtime::deterministic::Runner;

struct CertificationFixture {
    marshal: crate::marshal_types::MarshalMailbox,
    app: OutbeApplication<commonware_runtime::deterministic::Context>,
    _resolver: handler::Handler<Digest>,
    actor: commonware_runtime::Handle<()>,
}

impl CertificationFixture {
    async fn open(context: &commonware_runtime::deterministic::Context, partition: &str) -> Self {
        Self::open_with_buffer(context, partition, EmptyMarshalBuffer::default()).await
    }

    async fn open_with_buffer<B>(
        context: &commonware_runtime::deterministic::Context,
        partition: &str,
        buffer: B,
    ) -> Self
    where
        B: Buffer<crate::marshal_types::Variant, PublicKey = bls12381::PublicKey>,
    {
        let (marshal, resolver, actor) = start_marshal_in_partition(
            context.child("marshal"),
            HybridSchemeProvider::new(),
            buffer,
            partition.into(),
        )
        .await;
        let (app, _rx) = OutbeApplication::new(context.child("app"), 16, marshal.clone());
        Self {
            marshal,
            app,
            _resolver: resolver,
            actor,
        }
    }
}

#[test]
fn certify_waits_for_the_exact_candidate_without_a_local_verify() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "certification-exact").await;
        let round = Round::new(Epoch::new(0), View::new(2));
        let other = consensus_block_with_number(0x11, 7);
        assert!(fixture.marshal.verified(round, other).await);
        let block = consensus_block_with_number(0x12, 7);
        let digest = block.digest();

        let mut certification = fixture.app.certify(round, digest).await;
        context.sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            certification.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(fixture.marshal.verified(round, block).await);
        assert!(certification.await.expect("durable candidate must certify"));
    });
}

#[test]
fn closed_marshal_cannot_authorize_certification() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "certification-closed").await;
        fixture.actor.abort();
        let _ = fixture.actor.await;
        let certification = fixture.app.certify(Round::zero(), Digest::ZERO).await;
        assert!(
            certification.await.is_err(),
            "shutdown must not be a true or false vote"
        );
    });
}

#[test]
fn cancelled_certification_does_not_poison_later_recovery() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "certification-cancel").await;
        let round = Round::new(Epoch::new(0), View::new(3));
        let block = consensus_block_with_number(0x13, 8);
        let digest = block.digest();
        let cancelled = fixture.app.certify(round, digest).await;
        drop(cancelled);
        context.sleep(Duration::from_millis(50)).await;
        assert!(fixture.marshal.verified(round, block).await);
        assert!(fixture.app.certify(round, digest).await.await.unwrap());
    });
}

#[test]
fn certified_candidate_survives_an_unclean_restart() {
    let round = Round::new(Epoch::new(0), View::new(4));
    let (digest, checkpoint) =
        Runner::timed(Duration::from_secs(30)).start_and_recover(|context| async move {
            let block = consensus_block_with_number(0x14, 9);
            let digest = block.digest();
            let buffer = RecordingMarshalBuffer {
                available: Some(Arc::new(block)),
                ..Default::default()
            };
            let mut fixture =
                CertificationFixture::open_with_buffer(&context, "certification-restart", buffer)
                    .await;
            // No verified write pre-seeds the archive. Certification itself must
            // persist this network-buffer-only candidate before authorizing a vote.
            assert!(fixture.app.certify(round, digest).await.await.unwrap());
            digest
        });
    Runner::from(checkpoint).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "certification-restart").await;
        let block = fixture
            .marshal
            .subscribe_by_digest(
                digest,
                commonware_consensus::marshal::core::DigestFallback::FetchByRound { round },
            )
            .await
            .expect("certified block must survive a crash");
        assert_eq!(block.digest(), digest);
        assert!(fixture.app.certify(round, digest).await.await.unwrap());
    });
}

#[test]
fn a_closed_marshal_withholds_a_locally_built_proposal() {
    use crate::application::handler::proposal::ProposeOutcome;
    use outbe_primitives::projection::ExecutionReadBudget;
    use reth_ethereum::node::api::BeaconEngineMessage;
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let fixture = CertificationFixture::open(&context, "publication-closed").await;
        fixture.actor.abort();
        let _ = fixture.actor.await;
        let mut shared = finalizer_test_shared(fixture.marshal, HybridSchemeProvider::new());
        let (tx, mut engine) = tokio::sync::mpsc::unbounded_channel();
        shared.shared.engine = super::super::EngineHandle::new(tx);
        let block = consensus_block_with_number(0x15, 10);
        let publication = shared.publish_built_proposal(
            Round::new(Epoch::new(0), View::new(5)),
            (block.digest(), block),
            ExecutionReadBudget::new(),
        );
        let execution = async {
            let BeaconEngineMessage::NewPayload { tx, .. } = engine.recv().await.unwrap() else {
                panic!("candidate must be execution validated before publication");
            };
            tx.send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Valid)))
                .unwrap();
        };
        let (outcome, ()) = futures::join!(publication, execution);
        assert!(outcome.is_err());
        assert!(!matches!(outcome, Ok(ProposeOutcome::Proposed(_))));
    });
}
