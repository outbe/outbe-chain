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
