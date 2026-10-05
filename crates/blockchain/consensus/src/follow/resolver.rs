//! Follower resolver: serves the marshal's gap-repair backfill requests from
//! the local execution layer and an upstream node, WITHOUT P2P.
//!
//! The marshal issues backfill [`Request`](handler::Request)s through a
//! [`TargetedResolver`]. In the validator path that resolver is
//! `commonware_resolver::p2p`, which talks to consensus peers. A follower has no
//! consensus peers, so this resolver instead:
//!
//! * `Request::Block(digest)` -> reads the block from the local EL
//!   ([`LocalBlockSource`]) and delivers it back to the marshal.
//! * `Request::Finalized { height }` -> fetches the certificate + block from the
//!   upstream ([`FinalizedSource`]) and delivers the concatenated
//!   `(Finalization, ConsensusBlock)` bytes, which the marshal decodes and
//!   verifies against the epoch committee.
//! * `Request::Notarized { .. }` -> ignored (the follower only consumes finalized
//!   data; notarizations are a validator-internal concern).
//!
//! **Actor + mailbox split.** The runtime's `Context` is not `Clone`, but the
//! marshal requires the resolver it holds to be `Clone`. So the spawnable half
//! (which owns the context + sources) is a [`ResolverActor`] spawned once, and
//! the marshal-facing half is [`FollowResolver`] - a cheap `Clone` mailbox that
//! forwards each fetch to the actor over an unbounded channel. This mirrors the
//! p2p resolver's `Engine` + `Mailbox` shape.
//!
//! Delivery is via the marshal's [`handler::Handler`] (a `Consumer`): the actor
//! resolves the value bytes, then calls
//! [`Consumer::deliver`](commonware_resolver::Consumer::deliver) so the marshal
//! validates and stores it. This is the same `Handler`/`Receiver` pair the
//! marshal `start` consumes, obtained from [`handler::init`].

mod resolution;

pub(super) use resolution::FetchResolution;

use commonware_actor::Feedback;
use commonware_codec::Encode as _;
use commonware_consensus::marshal::resolver::handler::{self, Annotation, Key};
use commonware_consensus::types::Height;
use commonware_cryptography::bls12381;
use commonware_resolver::{Delivery, Fetch, Resolver, TargetedResolver};
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::vec::NonEmptyVec;
use futures::StreamExt as _;
use tracing::{debug, warn};

use crate::digest::Digest;
use crate::follow::upstream::{FinalizedSource, LocalBlockSource};
use crate::follow::{FollowerEpocher, SharedCommitteeChain};

/// The marshal backfill key type for outbe blocks (commitment = block digest).
pub(super) type ResolverKey = Key<Digest>;

type FetchTx = futures::channel::mpsc::UnboundedSender<Fetch<ResolverKey, Annotation>>;
type FetchRx = futures::channel::mpsc::UnboundedReceiver<Fetch<ResolverKey, Annotation>>;

/// The marshal-facing resolver: a cheap `Clone` mailbox forwarding fetches to
/// the spawned [`ResolverActor`]. Implements [`TargetedResolver`].
#[derive(Clone)]
pub(super) struct FollowResolver {
    tx: FetchTx,
}

/// The spawned half of the resolver: owns the context + sources and resolves
/// each fetch, delivering the result to the marshal's `Handler`.
pub(super) struct ResolverActor<E, F, L> {
    context: E,
    handler: handler::Handler<Digest>,
    upstream: F,
    local: L,
    /// Shared committee-chaining verifier. A finalized fetch is independently
    /// authenticated before any pre-announce or boundary observation is applied;
    /// the marshal then verifies the same certificate again on delivery.
    chain: SharedCommitteeChain,
    /// Shared authenticated height-to-epoch map used by the marshal.
    epocher: FollowerEpocher,
    rx: FetchRx,
}

/// Build the resolver actor + its marshal-facing mailbox.
pub(super) fn init<E, F, L>(
    context: E,
    handler: handler::Handler<Digest>,
    resolution: FetchResolution<F, L>,
) -> (ResolverActor<E, F, L>, FollowResolver) {
    let FetchResolution {
        upstream,
        local,
        chain,
        epocher,
    } = resolution;
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let actor = ResolverActor {
        context,
        handler,
        upstream,
        local,
        chain,
        epocher,
        rx,
    };
    (actor, FollowResolver { tx })
}

impl<E, F, L> ResolverActor<E, F, L>
where
    E: Spawner + Clock + Metrics + Send + Sync + 'static,
    F: FinalizedSource,
    L: LocalBlockSource,
{
    /// Spawn the actor's receive loop. Each fetch is resolved on its own child
    /// task so a slow upstream fetch never blocks others.
    pub(super) fn start(self) -> commonware_runtime::Handle<()> {
        self.context
            .child("follow_resolver")
            .spawn(move |_| async move {
                let ResolverActor {
                    context,
                    handler,
                    upstream,
                    local,
                    chain,
                    epocher,
                    mut rx,
                } = self;
                while let Some(fetch) = rx.next().await {
                    let task_ctx = context.child("follow_fetch");
                    let handler = handler.clone();
                    let upstream = upstream.clone();
                    let local = local.clone();
                    let chain = chain.clone();
                    let epocher = epocher.clone();
                    task_ctx.spawn(move |_| {
                        FetchResolution {
                            upstream,
                            local,
                            chain,
                            epocher,
                        }
                        .resolve(fetch, handler)
                    });
                }
            })
    }
}

/// The block height a `Request::Block` annotation pins, if any. Height-bound
/// annotations (`Certified { height }`, `Finalized(ByHeight { height })`) map a
/// block-commitment request to an upstream `getConsensusBlock(height)`. Round-bound
/// annotations carry no height and return `None`.
fn block_request_height(annotation: &Annotation) -> Option<Height> {
    match annotation {
        Annotation::Certified { height } => Some(*height),
        Annotation::Finalized(handler::Finalized::ByHeight { height }) => Some(*height),
        Annotation::Finalized(handler::Finalized::ByRound { .. })
        | Annotation::Notarization { .. } => None,
    }
}

impl FollowResolver {
    fn enqueue(&self, fetch: Fetch<ResolverKey, Annotation>) -> Feedback {
        match self.tx.unbounded_send(fetch) {
            Ok(()) => Feedback::Ok,
            Err(_) => Feedback::Closed,
        }
    }
}

impl Resolver for FollowResolver {
    type Key = ResolverKey;
    type Subscriber = Annotation;

    fn fetch<Fr>(&mut self, key: Fr) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        self.enqueue(key.into())
    }

    fn fetch_all<Fr>(&mut self, keys: Vec<Fr>) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        let mut feedback = Feedback::Ok;
        for key in keys {
            if self.enqueue(key.into()) == Feedback::Closed {
                feedback = Feedback::Closed;
            }
        }
        feedback
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        // Each fetch is a fire-and-forget task that either delivers or drops;
        // there is no persistent in-flight request table to prune. A task whose
        // height is already processed has its delivery ignored by the marshal as
        // stale, so retain is a no-op. (A cancellation table can be added later
        // if long gaps prove too chatty; it is not required for correctness.)
        Feedback::Ok
    }
}

impl TargetedResolver for FollowResolver {
    type PublicKey = bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        fetch: impl Into<Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: NonEmptyVec<bls12381::PublicKey>,
    ) -> Feedback {
        // The follower has a single upstream; target hints (which consensus peer
        // to ask) are meaningless here. Resolve from the upstream/EL regardless.
        self.enqueue(fetch.into())
    }

    fn fetch_all_targeted<Fr>(
        &mut self,
        keys: Vec<(Fr, NonEmptyVec<bls12381::PublicKey>)>,
    ) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        let mut feedback = Feedback::Ok;
        for (key, _targets) in keys {
            if self.enqueue(key.into()) == Feedback::Closed {
                feedback = Feedback::Closed;
            }
        }
        feedback
    }
}
