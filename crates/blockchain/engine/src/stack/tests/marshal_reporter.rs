use super::*;
use commonware_consensus::simplex::types::{Notarization, Notarize};
use commonware_utils::iter::NonEmpty;

struct MarshalActivities {
    provider: HybridSchemeProvider<MinSig>,
    finalization: Finalization<HybridScheme<MinSig>, outbe_consensus::digest::Digest>,
    notarization: Notarization<HybridScheme<MinSig>, outbe_consensus::digest::Digest>,
    finalize: Finalize<HybridScheme<MinSig>, outbe_consensus::digest::Digest>,
}

fn marshal_activity_fixture(block: &ConsensusBlock, round: Round) -> MarshalActivities {
    let keys: Vec<bls12381::PrivateKey> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg(3).unwrap();
    let signers = bootstrap_fixture_signers(&keys, &participants, &dkg);
    let verifier = HybridScheme::<MinSig>::verifier(
        &config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .unwrap();
    let proposal = Proposal::new(
        round,
        round.view().previous().unwrap_or(View::zero()),
        block.digest(),
    );
    let notarizes: Vec<_> = signers
        .iter()
        .map(|scheme| Notarize::sign(scheme, proposal.clone()).unwrap())
        .collect();
    let notarization = Notarization::from_notarizes(
        &verifier,
        NonEmpty::try_new(notarizes.iter()).unwrap(),
        &Sequential,
    )
    .unwrap();
    let finalization = certify_fixture_proposal(&verifier, &signers, proposal.clone());
    let finalize = Finalize::sign(&signers[0], proposal).unwrap();
    let provider = HybridSchemeProvider::new();
    let _ = provider.register(round.epoch(), verifier);
    MarshalActivities {
        provider,
        finalization,
        notarization,
        finalize,
    }
}

#[test]
fn marshal_returns_ok_for_certification_and_forwards_only_notarization_and_finalization() {
    let observed = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(1175));
        let block = recovery_block(5700);
        let activities = marshal_activity_fixture(&block, round);
        let (mut mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, activities.provider).await;
        let finalization = activities.finalization;
        let notarization = activities.notarization;
        let finalize = activities.finalize;

        assert!(mailbox.verified(round, block).await);
        assert_eq!(
            mailbox.report(Activity::Certification(notarization.clone())),
            Feedback::Ok
        );
        assert!(mailbox.get_finalization(Height::new(5700)).await.is_none());
        assert_eq!(
            mailbox.report(Activity::Finalization(finalization.clone())),
            Feedback::Ok
        );
        let stored = mailbox.get_finalization(Height::new(5700)).await;
        let stored_round = stored
            .as_ref()
            .map(|finalization| finalization.proposal.round);

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;

        let certification = mailbox.report(Activity::Certification(notarization.clone()));
        let raw_finalize = mailbox.report(Activity::Finalize(finalize));
        let notarization = mailbox.report(Activity::Notarization(notarization));
        let finalization = mailbox.report(Activity::Finalization(finalization));
        (
            stored_round,
            certification,
            raw_finalize,
            notarization,
            finalization,
        )
    });

    assert_eq!(observed.0, Some(Round::new(Epoch::new(0), View::new(1175))));
    assert_eq!(observed.1, Feedback::Ok);
    assert_eq!(observed.2, Feedback::Ok);
    assert_eq!(observed.3, Feedback::Closed);
    assert_eq!(observed.4, Feedback::Closed);
}
