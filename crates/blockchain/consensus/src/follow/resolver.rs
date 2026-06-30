//! Follower resolver: serves the marshal's gap-repair backfill requests from
//! the local execution layer and an upstream node, WITHOUT P2P.
//!
//! The marshal issues backfill [`Request`](handler::Request)s through a
//! [`TargetedResolver`]. In the validator path that resolver is
//! `commonware_resolver::p2p`, which talks to consensus peers. A follower has
//! no consensus peers, so this resolver instead:
//!
//! * `Request::Block(digest)` → reads the block from the local EL
//!   ([`LocalBlockSource`]) and delivers it back to the marshal.
//! * `Request::Finalized { height }` → fetches the certificate + block from the
//!   upstream ([`FinalizedSource`]) and delivers the concatenated
//!   `(Finalization, ConsensusBlock)` bytes, which the marshal decodes and
//!   verifies against the epoch committee.
//! * `Request::Notarized { .. }` → ignored (the follower only consumes
//!   finalized data; notarizations are a validator-internal concern).
//!
//! Delivery is via the marshal's [`handler::Handler`] (a `Consumer`): a fetch
//! task resolves the value bytes, then calls
//! [`Consumer::deliver`](commonware_resolver::Consumer::deliver) so the marshal
//! validates and stores it. This is the same `Handler`/`Receiver` pair the
//! marshal `start` consumes, obtained from [`handler::init`].

use std::sync::{Arc, Mutex};

use alloy_consensus::BlockHeader as _;
use commonware_codec::Encode as _;
use commonware_consensus::marshal::resolver::handler::{self, Annotation, Key};
use commonware_cryptography::bls12381;
use commonware_actor::Feedback;
use commonware_resolver::{Consumer as _, Delivery, Fetch, Resolver, TargetedResolver};
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::vec::NonEmptyVec;
use tracing::{debug, warn};

use crate::digest::Digest;
use crate::follow::upstream::{CertifiedFinalizedBlock, FinalizedSource, LocalBlockSource};
use crate::follow::CommitteeChain;

/// The marshal backfill key type for outbe blocks (commitment = block digest).
pub(super) type ResolverKey = Key<Digest>;

/// A follower resolver wired to a local EL block source and an upstream
/// finalized-block source.
///
/// Cloning is cheap (the sources are `Clone` handles and the context is a
/// runtime context clone) — required because the marshal holds the resolver by
/// value and the trait demands `Clone`.
pub(super) struct FollowResolver<E, F, L> {
    context: E,
    handler: handler::Handler<Digest>,
    upstream: F,
    local: L,
    /// Shared committee-chaining verifier. A `Finalized` fetch registers the
    /// fetched block's epoch committee (from its `extra_data`) BEFORE delivering
    /// the value to the marshal, so the marshal's per-epoch verifier provider
    /// (the same `CommitteeChain` provider) can verify that block's certificate.
    /// This closes the boundary-block circularity: the block that *announces*
    /// epoch N's committee is also the first block *signed by* epoch N.
    chain: Arc<Mutex<CommitteeChain>>,
}

impl<E, F, L> Clone for FollowResolver<E, F, L>
where
    E: Clone,
    F: Clone,
    L: Clone,
{
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
            handler: self.handler.clone(),
            upstream: self.upstream.clone(),
            local: self.local.clone(),
            chain: self.chain.clone(),
        }
    }
}

impl<E, F, L> FollowResolver<E, F, L>
where
    E: Spawner + Clock + Metrics + Clone + Send + Sync + 'static,
    F: FinalizedSource,
    L: LocalBlockSource,
{
    pub(super) fn new(
        context: E,
        handler: handler::Handler<Digest>,
        upstream: F,
        local: L,
        chain: Arc<Mutex<CommitteeChain>>,
    ) -> Self {
        Self {
            context,
            handler,
            upstream,
            local,
            chain,
        }
    }

    /// Spawn a task that resolves a single fetch and delivers it to the marshal.
    fn schedule(&self, fetch: Fetch<ResolverKey, Annotation>) {
        let Fetch { key, subscriber } = fetch;
        let mut handler = self.handler.clone();
        let upstream = self.upstream.clone();
        let local = self.local.clone();
        let chain = self.chain.clone();
        self.context
            .clone()
            .child("follow_fetch")
            .spawn(move |_| async move {
                let value = match &key {
                    Key::Block(commitment) => match local.get_block_by_digest(*commitment).await {
                        Some(block) => block.encode(),
                        None => {
                            debug!(%key, "local EL did not have requested block; dropping fetch");
                            return;
                        }
                    },
                    Key::Finalized { height } => {
                        match upstream.get_finalization(*height).await {
                            Some(CertifiedFinalizedBlock {
                                finalization,
                                block,
                            }) => {
                                // Register this block's epoch committee BEFORE
                                // delivering, so the marshal can verify the
                                // finalization. The boundary block that announces
                                // epoch N's committee is itself the first block
                                // signed by epoch N — without this lead-in the
                                // marshal would reject it for "no verifier".
                                //
                                // Trust is preserved: the marshal still verifies
                                // the certificate against the registered
                                // committee, and the committee is only ever read
                                // from a block the prior (trusted) committee's
                                // chain leads to. A non-boundary block registers
                                // nothing (returns Ok(None)).
                                let extra = block.header().extra_data().clone();
                                if let Err(error) = chain
                                    .lock()
                                    .expect("committee chain mutex poisoned")
                                    .advance_from_block_extra_data(extra.as_ref())
                                {
                                    warn!(%key, %error, "failed to register committee from fetched boundary block; dropping fetch");
                                    return;
                                }
                                // Wire format the marshal expects for a
                                // `Finalized` delivery: the finalization
                                // certificate immediately followed by the block.
                                let mut buf = finalization.encode().to_vec();
                                buf.extend_from_slice(block.encode().as_ref());
                                buf.into()
                            }
                            None => {
                                debug!(%key, "upstream did not have requested finalization; dropping fetch");
                                return;
                            }
                        }
                    }
                    Key::Notarized { .. } => {
                        debug!(%key, "ignoring notarized backfill request (follower)");
                        return;
                    }
                };

                let delivery = Delivery {
                    key,
                    subscribers: NonEmptyVec::new(subscriber),
                };
                // The returned receiver resolves to whether the marshal accepted
                // the value; we do not retry on rejection (a rejected value means
                // the upstream served something inconsistent — the marshal will
                // re-request if it still needs the height).
                let _accepted = handler.deliver(delivery, value);
            });
    }
}

impl<E, F, L> Resolver for FollowResolver<E, F, L>
where
    E: Spawner + Clock + Metrics + Clone + Send + Sync + 'static,
    F: FinalizedSource,
    L: LocalBlockSource,
{
    type Key = ResolverKey;
    type Subscriber = Annotation;

    fn fetch<Fr>(&mut self, key: Fr) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        self.schedule(key.into());
        Feedback::Ok
    }

    fn fetch_all<Fr>(&mut self, keys: Vec<Fr>) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        for key in keys {
            self.schedule(key.into());
        }
        Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        // Each fetch is a fire-and-forget spawned task that either delivers or
        // drops; there is no persistent in-flight request table to prune. A
        // task whose height is already processed simply has its delivery
        // ignored by the marshal as stale. (If this proves too chatty under
        // long gaps, a cancellation table can be added; it is not required for
        // correctness.)
        Feedback::Ok
    }
}

impl<E, F, L> TargetedResolver for FollowResolver<E, F, L>
where
    E: Spawner + Clock + Metrics + Clone + Send + Sync + 'static,
    F: FinalizedSource,
    L: LocalBlockSource,
{
    type PublicKey = bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        fetch: impl Into<Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: NonEmptyVec<bls12381::PublicKey>,
    ) -> Feedback {
        // The follower has a single upstream; target hints (which consensus peer
        // to ask) are meaningless here. Resolve from the upstream/EL regardless.
        self.schedule(fetch.into());
        Feedback::Ok
    }

    fn fetch_all_targeted<Fr>(
        &mut self,
        keys: Vec<(Fr, NonEmptyVec<bls12381::PublicKey>)>,
    ) -> Feedback
    where
        Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        for (key, _targets) in keys {
            self.schedule(key.into());
        }
        Feedback::Ok
    }
}
