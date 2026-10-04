use super::*;
use commonware_consensus::types::{Epoch, View};
use commonware_runtime::{deterministic, Clock as _, Runner as _, Supervisor as _};
use std::time::Duration;

fn candidate(seed: u8) -> ConsensusBlock {
    let mut block = reth_ethereum::Block::default();
    block.header.number = 7;
    block.header.extra_data = alloy_primitives::Bytes::from(vec![seed]);
    ConsensusBlock::from_sealed(reth_ethereum::primitives::SealedBlock::seal_slow(
        block.map_header(outbe_primitives::OutbeHeader::new),
    ))
}

fn round(view: u64) -> Round {
    Round::new(Epoch::new(0), View::new(view))
}

/// Simulate marshal accepting the staged proposal and returning a controlled
/// sync handle. Production uses `proposed` or `verified_deferred` for this.
pub(crate) fn take_ack(
    publication: &ProposalPublication,
    round: Round,
    digest: Digest,
) -> Option<oneshot::Sender<Handle<()>>> {
    publication
        .state
        .lock()
        .entries
        .get_mut(&(round, digest))?
        .staged
        .take()
        .map(|staged| staged.ack)
}

fn gate(publication: &ProposalPublication, round: Round, digest: Digest) -> Option<Durability> {
    publication
        .state
        .lock()
        .entries
        .get(&(round, digest))
        .map(|entry| entry.durable.clone())
}

fn staged(context: &deterministic::Context, seed: u8) -> (ProposalPublication, Digest) {
    let publication = ProposalPublication::new(context.child("publication"));
    let block = candidate(seed);
    let digest = block.digest();
    assert!(publication.stage(round(2), block));
    (publication, digest)
}

#[test]
fn publication_gate_waits_for_both_acknowledgement_and_sync() {
    deterministic::Runner::default().start(|context| async move {
        let (publication, digest) = staged(&context, 1);
        let durable = gate(&publication, round(2), digest).unwrap();
        assert!(durable.clone().now_or_never().is_none());
        let (completion, receiver) = oneshot::channel();
        assert!(take_ack(&publication, round(2), digest)
            .unwrap()
            .send(Handle::from_receiver(receiver))
            .is_ok());
        context.sleep(Duration::from_millis(10)).await;
        assert!(
            durable.clone().now_or_never().is_none(),
            "ack alone is not durability"
        );
        completion.send(Ok(())).unwrap();
        assert!(durable.await);
        assert!(
            gate(&publication, round(2), digest).unwrap().await,
            "duplicate certification shares durability"
        );
    });
}

#[test]
fn publication_rejects_a_second_candidate_in_the_same_round() {
    deterministic::Runner::default().start(|context| async move {
        let (publication, digest) = staged(&context, 2);
        assert!(!publication.stage(round(2), candidate(3)));
        assert!(publication.contains_round(round(2)));
        assert_eq!(publication.state.lock().entries.len(), 1);
        assert!(publication
            .state
            .lock()
            .entries
            .contains_key(&(round(2), digest)));
    });
}

#[test]
fn publication_abandonment_is_not_a_durable_verdict() {
    for error in [None, Some(Error::Closed), Some(Error::Aborted)] {
        deterministic::Runner::default().start(|context| async move {
            let (publication, digest) = staged(&context, 4);
            let ack = take_ack(&publication, round(2), digest).unwrap();
            if let Some(error) = error {
                assert!(ack.send(Handle::ready(Err(error))).is_ok());
            } else {
                drop(ack);
            }
            assert!(!gate(&publication, round(2), digest).unwrap().await);
        });
    }
}

#[test]
fn publication_cleanup_uses_a_monotonic_finalized_round() {
    deterministic::Runner::default().start(|context| async move {
        let (publication, digest) = staged(&context, 5);
        assert!(publication.stage(round(5), candidate(5)));
        let retired = gate(&publication, round(2), digest).unwrap();
        publication.retire_through(round(3));
        assert!(!retired.await);
        assert_eq!(publication.state.lock().entries.len(), 1);
        publication.retire_through(round(1));
        assert!(!publication.stage(round(2), candidate(6)));
        assert!(
            publication.contains_round(round(5)),
            "later views must remain recoverable"
        );
        publication.retire_through(round(5));
        assert!(publication.state.lock().entries.is_empty());
    });
}

#[test]
#[should_panic(expected = "failed to sync proposal")]
fn publication_sync_failure_is_fatal_without_a_certification_request() {
    deterministic::Runner::default().start(|context| async move {
        let (publication, digest) = staged(&context, 7);
        assert!(take_ack(&publication, round(2), digest)
            .unwrap()
            .send(Handle::ready(Err(Error::WriteFailed)))
            .is_ok());
        context.sleep(Duration::from_secs(1)).await;
        panic!("sync failure was not observed");
    });
}
