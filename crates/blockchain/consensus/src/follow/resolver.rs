//! Follower acquisition through Commonware's opaque resolver, as in Tempo.
//! Opaque owns coalescing, retained subscribers, cancellation, retries and delivery.
//! This adapter only preserves Outbe's height-bound RPC lookup and authenticated
//! ancestor bundles. RPC work is limited by a semaphore and byte/timeout caps.
mod resolution;
#[cfg(test)]
mod tests;

use crate::digest::Digest;
use crate::follow::upstream::{FinalizedSource, LocalBlockSource};
use crate::follow::{FollowerEpocher, SharedCommitteeChain};
use bytes::{Buf, BufMut, Bytes};
use commonware_actor::Feedback;
use commonware_codec::{EncodeSize, Read, ReadExt as _, Write};
use commonware_consensus::marshal::resolver::handler::{self, Annotation, Key};
use commonware_consensus::types::Height;
use commonware_cryptography::bls12381;
use commonware_resolver::{opaque, Consumer, Delivery, Fetch, Resolver, TargetedResolver};
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::{channel::oneshot, vec::NonEmptyVec, Span};
pub(super) use resolution::FetchResolution;
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

pub(super) type ResolverKey = Key<Digest>;
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const RETRY_STATE_TTL: Duration = Duration::from_secs(60);
const ACQUISITION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ACTIVE_ACQUISITIONS: usize = 8;

/// A Block RPC is selected by both digest and height. Opaque's fetcher receives
/// only its key, so preserve the subscriber's height in the internal key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BoundKey {
    key: ResolverKey,
    height: Option<Height>,
}
impl BoundKey {
    fn new(key: ResolverKey, subscriber: &Annotation) -> Self {
        let height = if matches!(key, Key::Block(_)) {
            block_request_height(subscriber)
        } else {
            None
        };
        Self { key, height }
    }
}
impl fmt::Display for BoundKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{:?}", self.key, self.height)
    }
}
impl Write for BoundKey {
    fn write(&self, buf: &mut impl BufMut) {
        self.key.write(buf);
        self.height.write(buf);
    }
}
impl Read for BoundKey {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, commonware_codec::Error> {
        Ok(Self {
            key: ResolverKey::read(buf)?,
            height: Option::<Height>::read(buf)?,
        })
    }
}
impl EncodeSize for BoundKey {
    fn encode_size(&self) -> usize {
        self.key.encode_size() + self.height.encode_size()
    }
}
impl Span for BoundKey {}

#[derive(Clone)]
pub(super) struct FollowResolver {
    inner: opaque::Resolver<BoundKey, Annotation, bls12381::PublicKey>,
}

pub(super) fn init<E, F, L>(
    context: E,
    handler: handler::Handler<Digest>,
    resolution: FetchResolution<F, L>,
    mailbox_size: NonZeroUsize,
) -> FollowResolver
where
    E: Clock + Metrics + Spawner,
    F: FinalizedSource,
    L: LocalBlockSource,
{
    let fetcher = Fetcher {
        context: Arc::new(context.child("acquisition")),
        resolution,
        handler: handler.clone(),
        permits: Arc::new(Semaphore::new(MAX_ACTIVE_ACQUISITIONS)),
        retries: Arc::new(parking_lot::Mutex::new(RetryState::default())),
    };
    FollowResolver {
        inner: opaque::init(
            context.child("opaque"),
            fetcher,
            MarshalConsumer(handler),
            mailbox_size,
            RETRY_FLOOR,
        ),
    }
}

fn block_request_height(annotation: &Annotation) -> Option<Height> {
    match annotation {
        Annotation::Certified { height }
        | Annotation::Finalized(handler::Finalized::ByHeight { height }) => Some(*height),
        Annotation::Finalized(handler::Finalized::ByRound { .. })
        | Annotation::Notarization { .. } => None,
    }
}
fn bind(fetch: Fetch<ResolverKey, Annotation>) -> Fetch<BoundKey, Annotation> {
    Fetch {
        key: BoundKey::new(fetch.key, &fetch.subscriber),
        subscriber: fetch.subscriber,
        span: fetch.span,
    }
}
impl Resolver for FollowResolver {
    type Key = ResolverKey;
    type Subscriber = Annotation;
    fn fetch<Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send>(
        &mut self,
        fetch: Fr,
    ) -> Feedback {
        self.inner.fetch(bind(fetch.into()))
    }
    fn fetch_all<Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send>(
        &mut self,
        fetches: Vec<Fr>,
    ) -> Feedback {
        self.inner
            .fetch_all(fetches.into_iter().map(|f| bind(f.into())).collect())
    }
    fn retain(
        &mut self,
        predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        self.inner
            .retain(move |key, subscriber| predicate(&key.key, subscriber))
    }
}
impl TargetedResolver for FollowResolver {
    type PublicKey = bls12381::PublicKey;
    fn fetch_targeted(
        &mut self,
        fetch: impl Into<Fetch<Self::Key, Self::Subscriber>> + Send,
        targets: NonEmptyVec<Self::PublicKey>,
    ) -> Feedback {
        self.inner.fetch_targeted(bind(fetch.into()), targets)
    }
    fn fetch_all_targeted<Fr: Into<Fetch<Self::Key, Self::Subscriber>> + Send>(
        &mut self,
        fetches: Vec<(Fr, NonEmptyVec<Self::PublicKey>)>,
    ) -> Feedback {
        self.inner.fetch_all_targeted(
            fetches
                .into_iter()
                .map(|(f, t)| (bind(f.into()), t))
                .collect(),
        )
    }
}

#[derive(Clone)]
enum Fetched {
    Value(Bytes),
    Delivered,
}
#[derive(Clone)]
struct Acquired {
    value: Fetched,
    _permit: Arc<OwnedSemaphorePermit>,
}
#[derive(Clone)]
struct MarshalConsumer(handler::Handler<Digest>);
impl Consumer for MarshalConsumer {
    type Key = BoundKey;
    type Value = Acquired;
    type Subscriber = Annotation;
    type Outcome = bool;
    fn deliver(
        &mut self,
        delivery: Delivery<BoundKey, Annotation>,
        value: Acquired,
    ) -> oneshot::Receiver<bool> {
        match value.value {
            Fetched::Value(bytes) => self.0.deliver(
                Delivery {
                    key: delivery.key.key,
                    subscribers: delivery.subscribers,
                },
                bytes,
            ),
            // The authenticated ancestor bundle has already been acknowledged by marshal.
            Fetched::Delivered => {
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(true);
                rx
            }
        }
    }
}
struct Fetcher<E, F, L> {
    context: Arc<E>,
    resolution: FetchResolution<F, L>,
    handler: handler::Handler<Digest>,
    permits: Arc<Semaphore>,
    retries: Arc<parking_lot::Mutex<RetryState>>,
}
impl<E, F: Clone, L: Clone> Clone for Fetcher<E, F, L> {
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
            resolution: self.resolution.clone(),
            handler: self.handler.clone(),
            permits: self.permits.clone(),
            retries: self.retries.clone(),
        }
    }
}
impl<E: Clock, F: FinalizedSource, L: LocalBlockSource> opaque::Fetcher for Fetcher<E, F, L> {
    type Key = BoundKey;
    type Value = Acquired;
    fn fetch(&self, key: BoundKey) -> impl Future<Output = Option<Acquired>> + Send {
        let this = self.clone();
        async move {
            if matches!(key.key, Key::Notarized { .. }) {
                return std::future::pending().await;
            }
            let delay = this.retries.lock().begin(key, this.context.current());
            if !delay.is_zero() {
                this.context.sleep(delay).await;
            }
            // Opaque queues/retries when all acquisition slots are occupied; no application scheduler.
            let permit = this.permits.try_acquire_owned().ok()?;
            let mut handler = this.handler;
            let span = tracing::info_span!("follow.acquire", key = %key);
            let resolution = this.resolution;
            let acquisition = async move { resolution.acquire(key, &span, &mut handler).await };
            let value = this
                .context
                .timeout(ACQUISITION_TIMEOUT, acquisition)
                .await
                .ok()
                .flatten();
            let mut retries = this.retries.lock();
            if value.is_some() {
                retries.entries.remove(&key);
            } else {
                retries.failed(key, delay, this.context.current());
            }
            value.map(|value| Acquired {
                value,
                _permit: Arc::new(permit),
            })
        }
    }
}

/// Same source-failure backoff as Tempo. Retry scheduling itself belongs to opaque.
/// Idle state expires because retain cancellation does not call the fetcher back.
#[derive(Default)]
struct RetryState {
    entries: BTreeMap<BoundKey, (Duration, SystemTime)>,
}
impl RetryState {
    fn begin(&mut self, key: BoundKey, now: SystemTime) -> Duration {
        self.entries.retain(|_, (_, used)| {
            now.duration_since(*used)
                .is_ok_and(|age| age < RETRY_STATE_TTL)
        });
        let entry = self.entries.entry(key).or_insert((Duration::ZERO, now));
        entry.1 = now;
        entry.0
    }
    fn failed(&mut self, key: BoundKey, used: Duration, now: SystemTime) {
        let delay = if used.is_zero() {
            RETRY_FLOOR
        } else {
            used.saturating_mul(2).min(MAX_RETRY_DELAY)
        };
        self.entries.insert(key, (delay, now));
    }
}
