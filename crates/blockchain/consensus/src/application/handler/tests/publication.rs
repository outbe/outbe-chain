use super::certification::CertificationFixture;
use super::*;
use crate::application::handler::proposal::{ProposalRequest, ProposeOutcome};
use commonware_consensus::{CertifiableAutomaton as _, Relay as _};
use commonware_runtime::deterministic::Runner;
use outbe_primitives::projection::ExecutionReadBudget;

fn request(round: Round, clock: &impl commonware_runtime::Clock) -> ProposalRequest {
    let (keys, _) = crate::test_fixtures::participants();
    ProposalRequest {
        context: crate::application::ingress::SimplexContext {
            round,
            parent: (View::new(0), Digest::ZERO),
            leader: keys[0].public_key(),
        },
        propose_start: clock.current(),
        execution_read_budget: ExecutionReadBudget::new(),
        payload_trace: Default::default(),
    }
}

#[test]
fn staged_publication_releases_the_digest_before_persistence_and_flushes_without_relay() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let fixture = CertificationFixture::open(&context, "staged-no-relay").await;
        let round = Round::new(Epoch::new(0), View::new(2));
        let block = consensus_block_with_number(0x31, 7);
        let digest = block.digest();
        let (shared, outcome) = fixture
            .publish_valid_candidate(&context, round, block)
            .await;
        assert_eq!(outcome.unwrap(), ProposeOutcome::Proposed(digest));
        assert!(
            fixture.marshal.get_verified(round).await.is_none(),
            "digest must precede marshal persistence"
        );
        assert_eq!(
            shared
                .handle_propose(&context, request(round, &context))
                .await
                .unwrap(),
            ProposeOutcome::RoundAlreadyProposed
        );

        // The original caller may discard its digest response and Simplex may
        // restart with another application clone; neither owns the staged block.
        let mut restarted = fixture.app.clone();
        drop(fixture.app);
        assert!(restarted.certify(round, digest).await.await.unwrap());
        assert!(restarted.certify(round, digest).await.await.unwrap());
        assert_eq!(
            fixture.marshal.get_verified(round).await.unwrap().digest(),
            digest
        );
    });
}

#[test]
fn staged_relay_preserves_targeted_recipients_and_allows_duplicate_forwarding() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let buffer = RecordingMarshalBuffer::default();
        let sends = buffer.sends.clone();
        let mut fixture =
            CertificationFixture::open_with_buffer(&context, "staged-relay", buffer).await;
        let round = Round::new(Epoch::new(0), View::new(2));
        let digest = fixture.stage_candidate(round, 0x32);
        let (keys, _) = crate::test_fixtures::participants();
        assert!(matches!(
            fixture.app.broadcast(
                digest,
                commonware_consensus::simplex::Plan::Forward {
                    round,
                    recipients: Recipients::Some(vec![keys[0].public_key()]),
                }
            ),
            Feedback::Ok
        ));
        assert!(fixture.app.certify(round, digest).await.await.unwrap());
        assert_eq!(*sends.lock().unwrap(), vec![(round, digest, false)]);
        assert!(matches!(
            fixture.app.broadcast(
                digest,
                commonware_consensus::simplex::Plan::Propose { round }
            ),
            Feedback::Ok
        ));
        // This mailbox query is ordered after the second relay.
        assert!(fixture.marshal.get_verified(round).await.is_some());
        assert_eq!(
            *sends.lock().unwrap(),
            vec![(round, digest, false), (round, digest, true)]
        );
    });
}

#[test]
fn wrong_identity_certification_cannot_consume_a_staged_candidate() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "staged-identity").await;
        let round = Round::new(Epoch::new(0), View::new(2));
        let wrong = Round::new(Epoch::new(0), View::new(3));
        let digest = fixture.stage_candidate(round, 0x33);
        for (other_round, other_digest) in [(wrong, digest), (round, Digest::ZERO)] {
            let mut certification = fixture.app.certify(other_round, other_digest).await;
            context.sleep(Duration::from_millis(50)).await;
            assert!(matches!(
                certification.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            assert!(fixture.marshal.get_verified(round).await.is_none());
            drop(certification);
        }
        assert!(fixture.app.certify(round, digest).await.await.unwrap());
    });
}

#[test]
fn persisted_round_guard_withholds_before_parent_resolution_or_engine_work() {
    let round = Round::new(Epoch::new(0), View::new(2));
    let (_, checkpoint) =
        Runner::timed(Duration::from_secs(30)).start_and_recover(|context| async move {
            let fixture = CertificationFixture::open(&context, "staged-round-restart").await;
            assert!(
                fixture
                    .marshal
                    .verified(round, consensus_block_with_number(0x34, 7))
                    .await
            );
        });
    Runner::from(checkpoint).start(|context| async move {
        let fixture = CertificationFixture::open(&context, "staged-round-restart").await;
        let shared = finalizer_test_shared(&context, fixture.marshal, HybridSchemeProvider::new());
        // The fixture engine is closed and the parent is unavailable. Only the
        // persisted-round guard can return this result before touching either.
        assert_eq!(
            shared
                .handle_propose(&context, request(round, &context))
                .await
                .unwrap(),
            ProposeOutcome::RoundAlreadyProposed
        );
    });
}

#[test]
fn staged_candidate_is_recoverable_after_certification_and_an_unclean_restart() {
    let round = Round::new(Epoch::new(0), View::new(2));
    let (digest, checkpoint) =
        Runner::timed(Duration::from_secs(30)).start_and_recover(|context| async move {
            let mut fixture = CertificationFixture::open(&context, "staged-crash").await;
            let digest = fixture.stage_candidate(round, 0x35);
            assert!(fixture.app.certify(round, digest).await.await.unwrap());
            digest
        });
    Runner::from(checkpoint).start(|context| async move {
        let mut fixture = CertificationFixture::open(&context, "staged-crash").await;
        // A new registry has no gate: the exact durable recovery path must work.
        assert!(fixture.app.certify(round, digest).await.await.unwrap());
        assert_eq!(
            fixture.marshal.get_verified(round).await.unwrap().digest(),
            digest
        );
    });
}

#[test]
fn staged_certification_waits_for_sync_after_an_earlier_request_is_cancelled() {
    Runner::timed(Duration::from_secs(30)).start(|context| async move {
        let mut fixture =
            CertificationFixture::open(&context, "staged-delayed-certification").await;
        let round = Round::new(Epoch::new(0), View::new(2));
        let digest = fixture.stage_candidate(round, 0x36);
        let publication = fixture.app.publication();
        let ack =
            crate::application::publication::tests::take_ack(&publication, round, digest).unwrap();
        let (completion, receiver) = oneshot::channel();
        assert!(ack
            .send(commonware_runtime::Handle::from_receiver(receiver))
            .is_ok());

        let mut abandoned = fixture.app.certify(round, digest).await;
        context.sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            abandoned.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(abandoned);
        let mut restarted = fixture.app.clone();
        let mut certification = restarted.certify(round, digest).await;
        context.sleep(Duration::from_millis(50)).await;
        assert!(
            matches!(
                certification.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "ack alone cannot authorize certify(true)"
        );
        completion.send(Ok(())).unwrap();
        assert!(certification.await.unwrap());
        assert!(restarted.certify(round, digest).await.await.unwrap());
    });
}
