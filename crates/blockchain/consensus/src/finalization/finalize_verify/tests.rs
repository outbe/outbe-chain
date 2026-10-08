use super::*;
use crate::{bls::bootstrap_dkg, test_fixtures::participants};
use commonware_consensus::{
    simplex::types::Proposal,
    types::{Round, View},
};
use commonware_utils::ordered::Quorum as _;

fn fixture() -> (
    Vec<HybridScheme<MinSig>>,
    HybridSchemeProvider<MinSig>,
    SharedLateFinalizeStore,
) {
    let (keys, participants) = participants();
    let dkg = bootstrap_dkg(3).unwrap();
    let signers = keys
        .iter()
        .map(|key| {
            let pk = commonware_cryptography::bls12381::PublicKey::from(key.clone());
            let index = participants.index(&pk).unwrap().get() as usize;
            HybridScheme::signer(
                b"bounded-finalize",
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[index].clone(),
            )
            .unwrap()
        })
        .collect();
    let verifier =
        HybridScheme::verifier(b"bounded-finalize", participants, dkg.polynomial).unwrap();
    let provider = HybridSchemeProvider::new();
    for epoch in 0..4 {
        assert!(provider.register(Epoch::new(epoch), verifier.clone()));
    }
    (
        signers,
        provider,
        crate::finalization::late_sig_store::shared(
            outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
        ),
    )
}

fn vote(signer: &HybridScheme<MinSig>, epoch: u64, view: u64, hash: u8) -> Vote {
    Finalize::sign(
        signer,
        Proposal::new(
            Round::new(Epoch::new(epoch), View::new(view)),
            View::new(view.saturating_sub(1)),
            Digest(B256::repeat_byte(hash)),
        ),
    )
    .unwrap()
}

#[test]
fn bounded_ingress_deduplicates_exact_bytes_and_schedules_other_signers() {
    let (signers, provider, store) = fixture();
    let (mut actor, mailbox) = FinalizeVerifyActor::new(provider, store.clone());
    let template = vote(&signers[0], 0, 1, 1);
    mailbox.verify(Epoch::new(0), template.clone());
    mailbox.verify(Epoch::new(0), template.clone());
    assert_eq!(actor.admission.lock().unwrap().depth().0, 1);
    for view in 2..1000 {
        let mut forged = template.clone();
        forged.proposal.round = Round::new(Epoch::new(0), View::new(view));
        mailbox.verify(Epoch::new(0), forged);
    }
    let honest = vote(&signers[1], 0, 1000, 2);
    mailbox.verify(Epoch::new(0), honest);
    let (count, bytes) = actor.admission.lock().unwrap().depth();
    assert!(count <= MAX_VOTES_PER_SIGNER + 1);
    assert!(count <= MAX_QUEUED_VOTES && bytes <= MAX_QUEUED_BYTES);
    assert!(actor.try_process_one()); // one item from attacker's bucket
    assert!(actor.try_process_one()); // round-robin gives honest signer a turn
    assert_eq!(
        store
            .lock()
            .unwrap()
            .pending_vote_count(B256::repeat_byte(2)),
        1
    );
}

#[test]
fn forged_before_valid_does_not_reserve_signer_target() {
    let (signers, provider, store) = fixture();
    let (mut actor, mailbox) = FinalizeVerifyActor::new(provider, store.clone());
    let valid = vote(&signers[0], 0, 7, 7);
    let mut forged = vote(&signers[0], 0, 8, 8);
    forged.proposal = valid.proposal.clone();
    mailbox.verify(Epoch::new(0), forged);
    mailbox.verify(Epoch::new(0), valid.clone());
    assert!(actor.try_process_one());
    assert_eq!(
        store
            .lock()
            .unwrap()
            .pending_vote_count(B256::repeat_byte(7)),
        0
    );
    assert!(actor.try_process_one());
    assert_eq!(
        store
            .lock()
            .unwrap()
            .pending_vote_count(B256::repeat_byte(7)),
        1
    );
    for _ in 0..10 {
        actor.verify_and_admit(Epoch::new(0), valid.clone());
    }
    assert_eq!(actor.observed_len(7), 1);
}

#[test]
fn verified_retention_tracks_epoch_and_view_watermarks_and_caps_conflicts() {
    let (signers, provider, store) = fixture();
    let (mut actor, _) = FinalizeVerifyActor::new(provider, store);
    for epoch in 0..4 {
        for hash in 1..=4 {
            actor.verify_and_admit(Epoch::new(epoch), vote(&signers[0], epoch, 7, hash));
        }
        assert!(actor.observed_finalizes.len() <= 2 * MAX_VERIFIED_CONFLICTS);
        assert!(actor.observed_bytes <= MAX_OBSERVED_BYTES);
    }
    assert!(actor.observed_finalizes.keys().all(|key| key.0 >= 2));
    actor.verify_and_admit(Epoch::new(3), vote(&signers[0], 3, 100, 100));
    actor.verify_and_admit(Epoch::new(3), vote(&signers[1], 3, 7, 7));
    assert!(!actor
        .observed_finalizes
        .keys()
        .any(|key| key.0 == 3 && key.1 == 7));
    let count = actor.observed_finalizes.len();
    actor.verify_and_admit(Epoch::new(0), vote(&signers[1], 0, 500, 50));
    assert_eq!(
        actor.observed_finalizes.len(),
        count,
        "old epochs cannot move retention backwards"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worker_yields_and_drains_bounded_queue_on_shutdown() {
    let (signers, provider, store) = fixture();
    let (actor, mailbox) = FinalizeVerifyActor::new(provider, store);
    for view in 1..=MAX_VOTES_PER_SIGNER as u64 {
        mailbox.verify(Epoch::new(0), vote(&signers[0], 0, view, view as u8));
    }
    let task = tokio::spawn(actor.run());
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "worker yields before draining its backlog"
    );
    drop(mailbox);
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn queue_overload_reclaims_capacity_and_remains_bounded_under_many_buckets() {
    let (signers, _, _) = fixture();
    let mut admission = Admission::default();
    // Exercise the queue's global budget independently of provider eligibility:
    // each bucket below is an actual encoded vote with a distinct epoch binding.
    // Provider membership is checked by the mailbox before reaching this seam.
    for epoch in 0..(MAX_QUEUED_VOTES as u64 * 3) {
        let finalize = vote(&signers[0], epoch, 1, 1);
        let encoded = finalize.encode();
        let key = (epoch, finalize.signer().get());
        assert!(admission.push(
            key,
            Queued {
                job: (Epoch::new(epoch), finalize),
                id: keccak256(&encoded),
                bytes: encoded.len()
            }
        ));
        let (count, bytes) = admission.depth();
        assert!(count <= MAX_QUEUED_VOTES && bytes <= MAX_QUEUED_BYTES);
    }
    assert_eq!(admission.depth().0, MAX_QUEUED_VOTES);
    let mut delivered = 0;
    let newest_epoch = MAX_QUEUED_VOTES as u64 * 3 - 1;
    let mut newest_delivered = false;
    while let Some(queued) = admission.pop() {
        delivered += 1;
        newest_delivered |= queued.job.0.get() == newest_epoch;
    }
    assert_eq!(delivered, MAX_QUEUED_VOTES);
    assert!(
        newest_delivered,
        "overload must not permanently reject new buckets"
    );
    assert_eq!(admission.depth(), (0, 0));
}
