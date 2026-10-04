//! Application actor - handles propose/verify/finalize via beacon_engine_handle.
//!
//! Implements the consensus `Automaton` and `Relay` traits, bridging
//! Commonware Simplex with Reth's execution layer.

use commonware_consensus::{Automaton, CertifiableAutomaton, Relay};
use commonware_cryptography::bls12381;
use commonware_p2p::Recipients;
use commonware_runtime::Spawner;
use commonware_utils::channel::oneshot;
use std::sync::Arc;

use super::ingress::{Mailbox, Message, SimplexContext};
use crate::digest::Digest;
use crate::marshal_types::MarshalMailbox;

/// The application actor that bridges consensus and execution.
///
/// Implements [`Automaton`]/[`CertifiableAutomaton`] so Simplex can call
/// `propose()`, `verify()`, and `certify()` (the genesis digest now feeds
/// `simplex::Config.floor` instead of an `Automaton::genesis` call).
/// Implements [`Relay`] so Simplex can broadcast proposals.
pub struct OutbeApplication<E> {
    context: Arc<E>,
    mailbox: Mailbox,
    publication: super::publication::ProposalPublication,
    /// Marshal mailbox used to disseminate a proposed block directly.
    ///
    /// Proposal construction registers the block and its durability barrier
    /// before releasing a digest. Certification awaits the barrier.
    /// Relay forwarding bypasses the bounded application mailbox.
    marshal_mailbox: MarshalMailbox,
}

impl<E> Clone for OutbeApplication<E> {
    fn clone(&self) -> Self {
        Self {
            context: Arc::clone(&self.context),
            mailbox: self.mailbox.clone(),
            publication: self.publication.clone(),
            marshal_mailbox: self.marshal_mailbox.clone(),
        }
    }
}

impl<E: Spawner> OutbeApplication<E> {
    /// Create a new application actor with its mailbox.
    pub fn new(
        context: E,
        mailbox_size: usize,
        marshal_mailbox: MarshalMailbox,
    ) -> (Self, futures::channel::mpsc::Receiver<Message>) {
        let (tx, rx) = futures::channel::mpsc::channel(mailbox_size);
        let mailbox = Mailbox::from_sender(tx);
        let publication =
            super::publication::ProposalPublication::new(context.child("publication"));
        (
            Self {
                context: Arc::new(context),
                mailbox,
                publication,
                marshal_mailbox,
            },
            rx,
        )
    }

    /// Get a clone of the mailbox for use by the reporter.
    pub fn reporter_mailbox(&self) -> Mailbox {
        self.mailbox.clone()
    }

    /// Shared proposal lifetime for the handler and finalized-tip reporter.
    pub fn publication(&self) -> super::publication::ProposalPublication {
        self.publication.clone()
    }
}

impl<E: Spawner> Automaton for OutbeApplication<E> {
    type Context = SimplexContext;
    type Digest = Digest;

    async fn propose(&mut self, context: Self::Context) -> oneshot::Receiver<Digest> {
        self.mailbox.propose(context).await
    }

    async fn verify(&mut self, context: Self::Context, payload: Digest) -> oneshot::Receiver<bool> {
        self.mailbox.verify(context, payload).await
    }
}

impl<E: Spawner> CertifiableAutomaton for OutbeApplication<E> {
    async fn certify(
        &mut self,
        round: commonware_consensus::types::Round,
        digest: Digest,
    ) -> oneshot::Receiver<bool> {
        let (response, receiver) = oneshot::channel();
        let marshal = self.marshal_mailbox.clone();
        let publication = self.publication.clone();
        self.context.child("certify").spawn(move |_| async move {
            super::certification::certify(marshal, publication, round, digest, response).await;
        });
        receiver
    }
}

impl<E: Spawner> Relay for OutbeApplication<E> {
    type Digest = Digest;
    type PublicKey = bls12381::PublicKey;
    type Plan = commonware_consensus::simplex::Plan<bls12381::PublicKey>;

    /// Disseminate a proposed block to the network.
    ///
    /// Hand a staged candidate directly to marshal, or forward its digest. Honor
    /// the relay plan's recipients without enqueueing an application message.
    fn broadcast(&mut self, payload: Self::Digest, plan: Self::Plan) -> commonware_actor::Feedback {
        // Honor the plan's intended recipients: `Propose` is a fresh broadcast to
        // all peers; `Forward` targets a specific subset (under ForwardPolicy
        // ::Disabled the batcher never emits `Forward`, but if a future policy
        // enables targeted forwarding we must NOT silently widen it to All).
        let (round, recipients) = match plan {
            commonware_consensus::simplex::Plan::Propose { round } => (round, Recipients::All),
            commonware_consensus::simplex::Plan::Forward { round, recipients } => {
                (round, recipients)
            }
        };
        tracing::debug!(payload = %payload.0, %round, "relay disseminating proposed block");
        self.publication
            .relay(&self.marshal_mailbox, (round, payload), recipients)
    }
}
