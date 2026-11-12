//! One marshal opener for engine tests and snapshot fixtures.
use crate::block::ConsensusBlock;
use crate::config;
use crate::hybrid::{HybridScheme, HybridSchemeProvider};
use commonware_consensus::{
    marshal::{self, resolver::handler, Start},
    types::ViewDelta,
};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_cryptography::certificate::Verifier as _;
use commonware_p2p::Recipients;
use commonware_parallel::Sequential;
use commonware_runtime::buffer::paged::CacheRef;
use commonware_runtime::Supervisor as _;
use commonware_storage::archive::immutable;
use commonware_utils::channel::oneshot;

use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct EmptyMarshalBuffer {
    pending_digest_subscribers: Arc<Mutex<Vec<oneshot::Sender<Arc<ConsensusBlock>>>>>,
    pending_commitment_subscribers: Arc<Mutex<Vec<oneshot::Sender<Arc<ConsensusBlock>>>>>,
}

impl marshal::core::Buffer<crate::marshal_types::Variant> for EmptyMarshalBuffer {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    async fn find_by_digest(&self, _digest: crate::digest::Digest) -> Option<Arc<ConsensusBlock>> {
        None
    }

    async fn find_by_commitment(
        &self,
        _commitment: crate::digest::Digest,
    ) -> Option<Arc<ConsensusBlock>> {
        None
    }

    fn subscribe_by_digest(
        &self,
        _digest: crate::digest::Digest,
    ) -> Option<oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (tx, rx) = oneshot::channel();
        self.pending_digest_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    fn subscribe_by_commitment(
        &self,
        _commitment: crate::digest::Digest,
    ) -> Option<oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (tx, rx) = oneshot::channel();
        self.pending_commitment_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    fn retire(&self, _update: marshal::core::Retirement<crate::digest::Digest>) {}

    fn send(
        &self,
        _round: commonware_consensus::types::Round,
        _block: Arc<ConsensusBlock>,
        _recipients: Recipients<Self::PublicKey>,
    ) {
    }
}

pub async fn start_recovery_marshal_in_partition<R>(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
    reporter: R,
    partition_prefix: String,
    genesis: ConsensusBlock,
) -> (
    crate::marshal_types::MarshalMailbox,
    handler::Handler<crate::digest::Digest>,
    commonware_runtime::Handle<()>,
)
where
    R: commonware_consensus::Reporter<Activity = crate::marshal_types::MarshalUpdate>
        + Send
        + 'static,
{
    assert!(!partition_prefix.is_empty());
    let page_cache = CacheRef::from_pooler(
        &context,
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let items_per_section = NonZeroU64::new(10).unwrap();
    let replay_buffer = NonZeroUsize::new(1024).unwrap();
    let write_buffer = NonZeroUsize::new(1024).unwrap();

    let finalizations_archive = immutable::Archive::init(
        context.child("recovery_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer,
            freezer_key_write_buffer: write_buffer,
            freezer_value_write_buffer: write_buffer,
            ordinal_write_buffer: write_buffer,
        },
    )
    .await
    .unwrap();

    let blocks_archive = immutable::Archive::init(
        context.child("recovery_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section,
            codec_config: (),
            replay_buffer,
            freezer_key_write_buffer: write_buffer,
            freezer_value_write_buffer: write_buffer,
            ordinal_write_buffer: write_buffer,
        },
    )
    .await
    .unwrap();

    let (actor, mailbox, _) = marshal::core::Actor::init(
        context.child("recovery_marshal"),
        finalizations_archive,
        blocks_archive,
        marshal::Config {
            provider,
            epocher: crate::follow::FollowerEpocher::new(10_000, 0),
            start: Start::Genesis(genesis),
            partition_prefix,
            mailbox_size: NonZeroUsize::new(32).unwrap(),
            view_retention: ViewDelta::new(10_000),
            prunable_items_per_section: items_per_section,
            page_cache,
            replay_buffer,
            key_write_buffer: write_buffer,
            value_write_buffer: write_buffer,
            block_codec_config: (),
            max_repair: NonZeroUsize::new(16).unwrap(),
            max_pending_acks: NonZeroUsize::new(16).unwrap(),
            strategy: Sequential,
        },
    )
    .await;

    let (resolver_rx, resolver_handler) = handler::init::<crate::digest::Digest>(
        context.child("resolver_handler"),
        NonZeroUsize::new(16).unwrap(),
    );
    let handle = actor.start(
        reporter,
        EmptyMarshalBuffer::default(),
        (resolver_rx, NoopMarshalResolver),
    );
    (mailbox, resolver_handler, handle)
}

use commonware_actor::Feedback;

use commonware_cryptography::bls12381;
use commonware_resolver::{Resolver, TargetedResolver};
use commonware_utils::vec::NonEmptyVec;

use crate::digest::Digest;

/// Accept requests without delivering blocks. The caller retains the handler keepalive.
#[derive(Clone, Default)]
struct NoopMarshalResolver;

// commonware 2026.5.0 split the resolver surface:
// - The base `Resolver` keeps `fetch`/`fetch_all`/`retain` (now SYNC, returning
//   `Feedback`, generic over `Into<Fetch<Key, Subscriber>>`) and gained
//   `type Subscriber`.
// - commonware 2026.5.0 removed `cancel`/`clear`.
// - The targeted methods moved to `TargetedResolver`.
// The marshal actor requires `Key = handler::Key<Commitment>` and `Subscriber =
// handler::Annotation`.
impl Resolver for NoopMarshalResolver {
    type Key = handler::Key<Digest>;
    type Subscriber = handler::Annotation;

    fn fetch<F>(&mut self, _key: F) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn fetch_all<F>(&mut self, _keys: Vec<F>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        Feedback::Ok
    }
}

impl TargetedResolver for NoopMarshalResolver {
    type PublicKey = bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        _fetch: impl Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: NonEmptyVec<Self::PublicKey>,
    ) -> Feedback {
        Feedback::Ok
    }

    fn fetch_all_targeted<F>(&mut self, _keys: Vec<(F, NonEmptyVec<Self::PublicKey>)>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }
}
