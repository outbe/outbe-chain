//! Gate local proposals and their publication, never imported-block verification.
use commonware_actor::Feedback;
use commonware_consensus::{simplex::Plan, types::Round, Automaton, CertifiableAutomaton, Relay};
use commonware_utils::channel::oneshot;
use std::sync::Arc;

pub(super) type Permission = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone)]
pub(super) struct ProposalGate<A> {
    inner: A,
    permission: Permission,
}
impl<A> ProposalGate<A> {
    pub(super) fn new(inner: A, permission: Permission) -> Self {
        Self { inner, permission }
    }
}
impl<A: Automaton> Automaton for ProposalGate<A>
where
    A::Context: Send,
{
    type Context = A::Context;
    type Digest = A::Digest;
    async fn propose(&mut self, context: Self::Context) -> oneshot::Receiver<Self::Digest> {
        if !(self.permission)() {
            return oneshot::channel().1;
        }
        self.inner.propose(context).await
    }
    async fn verify(
        &mut self,
        context: Self::Context,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        self.inner.verify(context, payload).await
    }
}
impl<A: CertifiableAutomaton> CertifiableAutomaton for ProposalGate<A>
where
    A::Context: Send,
{
    async fn certify(&mut self, round: Round, payload: Self::Digest) -> oneshot::Receiver<bool> {
        self.inner.certify(round, payload).await
    }
}
impl<A: Relay<Plan = Plan<commonware_cryptography::bls12381::PublicKey>>> Relay
    for ProposalGate<A>
{
    type Digest = A::Digest;
    type PublicKey = A::PublicKey;
    type Plan = A::Plan;
    fn broadcast(&mut self, payload: Self::Digest, plan: Self::Plan) -> Feedback {
        // Recheck at publication: an already-building proposal can outlive admission.
        if matches!(plan, Plan::Propose { .. }) && !(self.permission)() {
            return Feedback::Backoff;
        }
        self.inner.broadcast(payload, plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        digest::Digest,
        hybrid::test_support::{signer_schemes, test_participants, NAMESPACE},
    };
    use alloy_primitives::B256;
    use commonware_consensus::{
        simplex::types::Subject,
        types::{Epoch, View},
    };
    use commonware_cryptography::certificate::Scheme as _;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct Application(Arc<AtomicUsize>);
    impl Automaton for Application {
        type Context = ();
        type Digest = Digest;
        async fn propose(&mut self, _: ()) -> oneshot::Receiver<Digest> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Digest(B256::repeat_byte(9)));
            rx
        }
        async fn verify(&mut self, _: (), _: Digest) -> oneshot::Receiver<bool> {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(true);
            rx
        }
    }
    impl CertifiableAutomaton for Application {}
    impl Relay for Application {
        type Digest = Digest;
        type PublicKey = commonware_cryptography::bls12381::PublicKey;
        type Plan = Plan<Self::PublicKey>;
        fn broadcast(&mut self, _: Digest, _: Self::Plan) -> Feedback {
            self.0.fetch_add(1, Ordering::SeqCst);
            Feedback::Ok
        }
    }

    #[tokio::test]
    async fn same_epoch_suspend_resume_retains_wal_identity_without_signatures_or_proposals() {
        let channel = TeeAuthorityChannel::new(TeeAuthorityState::ArmedBootstrap {
            enclave_id: B256::repeat_byte(1),
        });
        let watch = channel.subscribe();
        let permission: Permission = Arc::new(move || watch.snapshot().is_authority_open());
        let (keys, participants) = test_participants(3);
        let dkg = crate::bls::bootstrap_dkg(3).unwrap();
        let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);
        let signing = permission.clone();
        let scheme = schemes[0].clone().with_signing_gate(move || signing());
        let identity = scheme.me();
        let round = Round::new(Epoch::new(1), View::new(2));
        let app = Application::default();
        let counter = app.0.clone();
        let mut app = ProposalGate::new(app, permission);
        let payload = app.propose(()).await.await.unwrap();
        assert!(scheme.sign::<Digest>(Subject::Nullify { round }).is_some());
        for suspended in [
            TeeAuthorityState::SuspendedJailed,
            TeeAuthorityState::SuspendedLeaseExpired,
            TeeAuthorityState::SuspendedPolicyMismatch,
            TeeAuthorityState::UnkeyedAwaitingAdmission,
        ] {
            channel.publish(suspended);
            assert_eq!(
                scheme.me(),
                identity,
                "Commonware WAL local-vote replay invariant"
            );
            assert!(scheme.sign::<Digest>(Subject::Nullify { round }).is_none());
            assert!(app.propose(()).await.await.is_err());
            assert_eq!(
                app.broadcast(payload, Plan::Propose { round }),
                Feedback::Backoff
            );
            assert_eq!(
                counter.load(Ordering::SeqCst),
                1,
                "no construction or publication while closed"
            );
            assert!(app.verify((), payload).await.await.unwrap());
            assert!(app.certify(round, payload).await.await.unwrap());
        }
        channel.publish(TeeAuthorityState::Armed {
            enclave_id: B256::repeat_byte(1),
            valid_until: 100,
        });
        assert_eq!(scheme.me(), identity);
        assert!(scheme.sign::<Digest>(Subject::Nullify { round }).is_some());
        assert!(app.propose(()).await.await.is_ok());
        assert_eq!(
            app.broadcast(payload, Plan::Propose { round }),
            Feedback::Ok
        );
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
