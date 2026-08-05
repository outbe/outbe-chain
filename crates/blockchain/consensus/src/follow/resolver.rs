//! Follower resolver: serves the marshal's gap-repair backfill requests from
//! the local execution layer and an upstream node, WITHOUT P2P.
//!
//! The marshal issues backfill [`Request`](handler::Request)s through a
//! [`TargetedResolver`]. In the validator path that resolver is
//! `commonware_resolver::p2p`, which talks to consensus peers. A follower has no
//! consensus peers, so this resolver instead:
//!
//! * `Request::Block(digest)` → reads the block from the local EL
//!   ([`LocalBlockSource`]) and delivers it back to the marshal.
//! * `Request::Finalized { height }` → fetches the certificate + block from the
//!   upstream ([`FinalizedSource`]) and delivers the concatenated
//!   `(Finalization, ConsensusBlock)` bytes, which the marshal decodes and
//!   verifies against the epoch committee.
//! * `Request::Notarized { .. }` → ignored (the follower only consumes finalized
//!   data; notarizations are a validator-internal concern).
//!
//! **Actor + mailbox split.** The runtime's `Context` is not `Clone`, but the
//! marshal requires the resolver it holds to be `Clone`. So the spawnable half
//! (which owns the context + sources) is a [`ResolverActor`] spawned once, and
//! the marshal-facing half is [`FollowResolver`] — a cheap `Clone` mailbox that
//! forwards each fetch to the actor over an unbounded channel. This mirrors the
//! p2p resolver's `Engine` + `Mailbox` shape.
//!
//! Delivery is via the marshal's [`handler::Handler`] (a `Consumer`): the actor
//! resolves the value bytes, then calls
//! [`Consumer::deliver`](commonware_resolver::Consumer::deliver) so the marshal
//! validates and stores it. This is the same `Handler`/`Receiver` pair the
//! marshal `start` consumes, obtained from [`handler::init`].

use std::sync::{Arc, Mutex};

use alloy_consensus::BlockHeader as _;
use commonware_actor::Feedback;
use commonware_codec::Encode as _;
use commonware_consensus::marshal::resolver::handler::{self, Annotation, Key};
use commonware_consensus::types::Height;
use commonware_cryptography::bls12381;
use commonware_resolver::{Consumer as _, Delivery, Fetch, Resolver, TargetedResolver};
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::vec::NonEmptyVec;
use futures::StreamExt as _;
use tracing::{debug, warn};

use crate::digest::Digest;
use crate::follow::upstream::{CertifiedFinalizedBlock, FinalizedSource, LocalBlockSource};
use crate::follow::{CommitteeChain, FollowerEpocher};

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
    /// Shared committee-chaining verifier. A `Finalized` fetch registers the
    /// fetched block's epoch committee (from its `extra_data`) BEFORE delivering
    /// the value to the marshal, so the marshal's per-epoch verifier provider
    /// (the same `CommitteeChain` provider) can verify that block's certificate.
    /// This closes the boundary-block circularity: the block that *announces*
    /// epoch N's committee is also the first block *signed by* epoch N.
    chain: Arc<Mutex<CommitteeChain>>,
    /// Observed epoch boundaries, shared with the marshal. Written here from
    /// each fetched certificate before the value is delivered.
    epoch_map: FollowerEpocher,
    rx: FetchRx,
}

/// Build the resolver actor + its marshal-facing mailbox.
pub(super) fn init<E, F, L>(
    context: E,
    handler: handler::Handler<Digest>,
    upstream: F,
    local: L,
    chain: Arc<Mutex<CommitteeChain>>,
    epoch_map: FollowerEpocher,
) -> (ResolverActor<E, F, L>, FollowResolver) {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let actor = ResolverActor {
        context,
        handler,
        upstream,
        local,
        chain,
        epoch_map,
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
                    epoch_map,
                    mut rx,
                } = self;
                while let Some(fetch) = rx.next().await {
                    let task_ctx = context.child("follow_fetch");
                    let handler = handler.clone();
                    let upstream = upstream.clone();
                    let local = local.clone();
                    let chain = chain.clone();
                    let epoch_map = epoch_map.clone();
                    task_ctx.spawn(move |_| {
                        resolve_one(fetch, handler, upstream, local, chain, epoch_map)
                    });
                }
            })
    }
}

/// Resolve a single fetch and deliver the value to the marshal.
async fn resolve_one<F, L>(
    fetch: Fetch<ResolverKey, Annotation>,
    mut handler: handler::Handler<Digest>,
    upstream: F,
    local: L,
    chain: Arc<Mutex<CommitteeChain>>,
    epoch_map: FollowerEpocher,
) where
    F: FinalizedSource,
    L: LocalBlockSource,
{
    let Fetch { key, subscriber } = fetch;
    debug!(%key, "resolver received fetch");
    let value = match &key {
        Key::Block(commitment) => {
            // First try the local EL (a block the follower already imported).
            if let Some(block) = local.get_block_by_digest(*commitment).await {
                block.encode()
            } else if let Some(height) = block_request_height(&subscriber) {
                // The follower has no block P2P, so a parent/ancestor block the
                // marshal needs for chain repair must come from the UPSTREAM.
                // The annotation carries the block's height; fetch that height's
                // finalized block and verify its digest matches the requested
                // commitment (the marshal re-checks too).
                match upstream.get_finalization(height).await {
                    Some(CertifiedFinalizedBlock { block, .. })
                        if block.digest() == *commitment =>
                    {
                        block.encode()
                    }
                    Some(_) => {
                        debug!(%key, %height, "upstream block at height did not match requested commitment; dropping fetch");
                        return;
                    }
                    None => {
                        debug!(%key, %height, "upstream did not have requested block; dropping fetch");
                        return;
                    }
                }
            } else {
                // A round-bound (`ByRound`/`Notarization`) block request has no
                // height; the follower cannot map it to an upstream height. The
                // marshal re-requests finalized-chain blocks by height, so this
                // is a benign drop.
                debug!(%key, "block request without a height annotation; dropping fetch");
                return;
            }
        }
        Key::Finalized { height } => match upstream.get_finalization(*height).await {
            Some(CertifiedFinalizedBlock {
                finalization,
                block,
            }) => {
                // Register this block's epoch committee BEFORE delivering, so the
                // marshal can verify the finalization. The boundary block that
                // announces epoch N's committee is itself the first block signed
                // by epoch N — without this lead-in the marshal would reject it
                // for "no verifier". Trust is preserved: the marshal still
                // verifies the certificate against the registered committee, and
                // the committee is only ever read from a block the prior
                // (trusted) committee's chain leads to. A non-boundary block
                // registers nothing (returns Ok(None)).
                let extra = block.header().extra_data().clone();
                {
                    let mut guard = chain.lock().expect("committee chain mutex poisoned");
                    if let Err(error) = guard.advance_from_block_extra_data(extra.as_ref()) {
                        warn!(%key, %error, "failed to register committee from fetched boundary block; dropping fetch");
                        return;
                    }
                }
                // Record where this epoch starts, so the marshal can map the
                // height back to it. The certificate names the epoch and this
                // fetch names the height; nothing else in the follower knows
                // both. It must happen BEFORE `deliver` — the marshal calls
                // `epocher.containing(height)` inside that call, and an empty
                // answer makes it drop the delivery silently.
                //
                // Recording every height (not only boundary blocks) is what
                // keeps a mid-chain restart working: the first fetch after the
                // recovered floor seeds the map without needing to find the
                // epoch's true first block first. `record` only ever lowers a
                // known boundary, so a later boundary block corrects it.
                epoch_map.record(*height, finalization.round().epoch());
                // Wire format the marshal expects for a `Finalized` delivery: the
                // finalization certificate immediately followed by the block.
                let mut buf = finalization.encode().to_vec();
                buf.extend_from_slice(block.encode().as_ref());
                buf.into()
            }
            None => {
                debug!(%key, "upstream did not have requested finalization; dropping fetch");
                return;
            }
        },
        Key::Notarized { .. } => {
            debug!(%key, "ignoring notarized backfill request (follower)");
            return;
        }
    };

    let value_len = value.len();
    let delivery = Delivery {
        key,
        subscribers: NonEmptyVec::new(subscriber),
    };
    // AWAIT the marshal's validation response. Dropping the returned receiver is
    // the resolver-protocol CANCELLATION signal: the marshal checks
    // `response.is_closed()` at dequeue and silently skips a delivery whose
    // receiver is gone (see `handler::Message::response_closed`). Since this
    // fetch runs on its own spawned task, holding the receiver open until the
    // marshal answers costs nothing — and the answer tells us whether the value
    // was accepted. We do not retry on rejection (the marshal re-requests if it
    // still needs the height).
    match handler.deliver(delivery, value).await {
        Ok(true) => debug!(%key, "delivery accepted by marshal"),
        Ok(false) => {
            let (registered, anchor) = {
                let guard = chain.lock().expect("committee chain mutex poisoned");
                (
                    guard.highest_registered().map(|e| e.get()),
                    guard.anchor_epoch(),
                )
            };
            warn!(
                %key,
                value_len,
                highest_registered = registered,
                anchor_epoch = anchor,
                "delivery rejected by marshal"
            );
        }
        Err(_) => debug!(%key, "marshal dropped delivery response (shutdown or batch prune)"),
    }
}

/// The block height a `Request::Block` annotation pins, if any. Height-bound
/// annotations (`Certified { height }`, `Finalized(ByHeight { height })`) map a
/// block-commitment request to an upstream `getFinalization(height)`. Round-bound
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::ConsensusBlock;
    use crate::follow::tests::{committee, Committee};
    use commonware_consensus::types::{Epoch, Epocher as _};
    use commonware_runtime::Runner as _;
    use futures::{pin_mut, poll};

    #[derive(Clone)]
    struct StubUpstream(Option<CertifiedFinalizedBlock>);

    impl FinalizedSource for StubUpstream {
        async fn get_finalization(&self, _height: Height) -> Option<CertifiedFinalizedBlock> {
            self.0.clone()
        }
    }

    #[derive(Clone)]
    struct NoLocalBlocks;

    impl LocalBlockSource for NoLocalBlocks {
        async fn get_block_by_digest(&self, _digest: Digest) -> Option<ConsensusBlock> {
            None
        }
    }

    fn certified(
        c: &Committee,
        epoch: Epoch,
        height: u64,
        extra: Vec<u8>,
    ) -> CertifiedFinalizedBlock {
        use alloy_primitives::Bytes;
        use outbe_primitives::OutbeHeader;
        use reth_ethereum::primitives::SealedBlock;
        use reth_ethereum::Block;

        let mut b = Block::default();
        b.header.number = height;
        b.header.extra_data = Bytes::from(extra);
        let block =
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(b.map_header(OutbeHeader::new)));
        CertifiedFinalizedBlock {
            finalization: c.finalization(epoch),
            block,
        }
    }

    fn finalized_fetch(height: u64) -> Fetch<ResolverKey, Annotation> {
        let height = Height::new(height);
        Fetch {
            key: Key::Finalized { height },
            subscriber: Annotation::Finalized(handler::Finalized::ByHeight { height }),
        }
    }

    /// Poll `resolve_one` far enough to reach `deliver`, which parks forever
    /// because nothing drains the marshal mailbox. Whatever the epoch map holds
    /// at that point is what the marshal would see inside `deliver`.
    fn state_at_delivery(
        certified: Option<CertifiedFinalizedBlock>,
        chain: Arc<Mutex<CommitteeChain>>,
        height: u64,
    ) -> FollowerEpocher {
        let epoch_map = FollowerEpocher::new();
        let observed = epoch_map.clone();
        commonware_runtime::deterministic::Runner::default().start(|context| async move {
            let (_receiver, handler) = handler::init::<Digest>(
                context,
                std::num::NonZeroUsize::new(4).expect("non-zero capacity"),
            );
            let fut = resolve_one(
                finalized_fetch(height),
                handler,
                StubUpstream(certified),
                NoLocalBlocks,
                chain,
                epoch_map,
            );
            pin_mut!(fut);
            let _ = poll!(fut.as_mut());
        });
        observed
    }

    /// The whole bug in one assertion: the marshal reads
    /// `epocher.containing(height)` INSIDE `deliver` and drops the delivery
    /// silently when it disagrees with the certificate. So the boundary must be
    /// recorded before `deliver` is ever called — recording after it would work
    /// in every test that inspects the map afterwards and still fail on the one
    /// block that matters, the boundary block.
    #[test]
    fn boundary_epoch_is_recorded_before_the_delivery_is_made() {
        let epoch = Epoch::new(186);
        let c = committee(20);
        let chain = Arc::new(Mutex::new(CommitteeChain::new(
            epoch,
            c.participants.clone(),
        )));
        let block = certified(&c, epoch, 223_123, c.boundary_block_extra_data(epoch));

        let map = state_at_delivery(Some(block), chain, 223_123);

        assert_eq!(
            map.containing(Height::new(223_123))
                .expect("the map must already answer for the delivered height")
                .epoch()
                .get(),
            186,
        );
    }

    /// A non-boundary block carries no committee artifact, but its certificate
    /// still names the epoch in force. Recording it is what lets a follower
    /// resuming mid-chain answer for heights before it ever reaches a boundary.
    #[test]
    fn a_block_without_a_boundary_artifact_still_records_its_epoch() {
        let epoch = Epoch::new(186);
        let c = committee(20);
        let chain = Arc::new(Mutex::new(CommitteeChain::new(
            epoch,
            c.participants.clone(),
        )));
        let block = certified(&c, epoch, 223_500, Vec::new());

        let map = state_at_delivery(Some(block), chain, 223_500);

        assert_eq!(
            map.containing(Height::new(223_500)).unwrap().epoch().get(),
            186,
        );
    }

    /// Committee registration failing aborts the fetch. Recording the boundary
    /// anyway would leave the map claiming an epoch whose committee was never
    /// registered — every later height in it would map to a verifier that does
    /// not exist.
    #[test]
    fn a_rejected_block_records_nothing() {
        let epoch = Epoch::new(186);
        let c = committee(20);
        let chain = Arc::new(Mutex::new(CommitteeChain::new(
            epoch,
            c.participants.clone(),
        )));
        let block = certified(&c, epoch, 223_123, vec![0xff, 0xff, 0xff, 0xff]);

        let map = state_at_delivery(Some(block), chain, 223_123);

        assert!(map.containing(Height::new(223_123)).is_none());
    }

    /// An upstream miss must not record anything either — the marshal will
    /// re-request the height, and a map entry with no delivered block behind it
    /// would answer for a height the follower has never verified.
    #[test]
    fn an_upstream_miss_records_nothing() {
        let epoch = Epoch::new(186);
        let c = committee(20);
        let chain = Arc::new(Mutex::new(CommitteeChain::new(epoch, c.participants)));

        let map = state_at_delivery(None, chain, 223_123);

        assert!(map.containing(Height::new(223_123)).is_none());
    }
}
