//! Shared archive settings for deterministic marshal test harnesses.

use eyre::WrapErr as _;
use std::num::{NonZeroU64, NonZeroUsize};

use commonware_cryptography::certificate::Verifier as _;
use commonware_runtime::buffer::paged::CacheRef;
use commonware_storage::archive::immutable;

pub(crate) enum MarshalArchiveKind {
    Finalizations,
    Blocks,
}

impl MarshalArchiveKind {
    fn partition_suffix(&self) -> &'static str {
        match self {
            Self::Finalizations => "finalizations",
            Self::Blocks => "blocks",
        }
    }
}

/// Archive configuration inputs shared by one marshal harness.
pub(crate) struct MarshalArchiveFixture<'a> {
    pub(crate) partition_prefix: &'a str,
    pub(crate) page_cache: &'a CacheRef,
    pub(crate) items_per_section: NonZeroU64,
    pub(crate) replay_buffer: NonZeroUsize,
    pub(crate) write_buffer: NonZeroUsize,
}

type MarshalTestArchives<E> = (
    immutable::Archive<E, crate::digest::Digest, crate::marshal_types::Finalization>,
    immutable::Archive<E, crate::digest::Digest, std::sync::Arc<crate::block::ConsensusBlock>>,
);

impl MarshalArchiveFixture<'_> {
    /// Open both archives in the same order and partitions used by every harness.
    pub(crate) async fn open<E>(&self, context: &E) -> eyre::Result<MarshalTestArchives<E>>
    where
        E: commonware_runtime::Storage + commonware_runtime::Clock + commonware_runtime::Metrics,
    {
        let finalizations = immutable::Archive::init(
            context.child("marshal_finalizations"),
            self.config(
                MarshalArchiveKind::Finalizations,
                crate::hybrid::HybridScheme::<
                    commonware_cryptography::bls12381::primitives::variant::MinSig,
                >::certificate_codec_config_unbounded(),
            ),
        )
        .await
        .wrap_err("finalizations archive should initialize")?;
        let blocks = immutable::Archive::init(
            context.child("marshal_blocks"),
            self.config(MarshalArchiveKind::Blocks, ()),
        )
        .await
        .wrap_err("blocks archive should initialize")?;
        Ok((finalizations, blocks))
    }

    pub(crate) fn config<C>(
        &self,
        kind: MarshalArchiveKind,
        codec_config: C,
    ) -> immutable::Config<C> {
        let prefix = self.partition_prefix;
        let suffix = kind.partition_suffix();
        immutable::Config {
            metadata_partition: format!("{prefix}-{suffix}-metadata"),
            freezer_table_partition: format!("{prefix}-{suffix}-freezer-table"),
            freezer_table_initial_size: 64,
            freezer_table_resize_frequency: 10,
            freezer_table_resize_chunk_size: 10,
            freezer_key_partition: format!("{prefix}-{suffix}-freezer-key"),
            freezer_key_page_cache: self.page_cache.clone(),
            freezer_value_partition: format!("{prefix}-{suffix}-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{prefix}-{suffix}-ordinal"),
            items_per_section: self.items_per_section,
            codec_config,
            replay_buffer: self.replay_buffer,
            freezer_key_write_buffer: self.write_buffer,
            freezer_value_write_buffer: self.write_buffer,
            ordinal_write_buffer: self.write_buffer,
        }
    }
}

/// Bounded deterministic polling helper: panics on timeout instead of hanging on regressions.
pub(crate) async fn wait_until_bounded<F, Fut>(
    context: &commonware_runtime::deterministic::Context,
    max_iters: usize,
    label: &str,
    mut condition: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    use commonware_runtime::Clock as _;
    for _ in 0..max_iters {
        if condition().await {
            return;
        }
        context.sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("timed out after {max_iters} iterations waiting for: {label}");
}

/// Marshal reporter that auto-acknowledges genesis (height 0) and holds subsequent block
/// acknowledgements so tests can observe `Processed::Absent(floor - 1)` before acking `floor`.
#[derive(Clone)]
pub(crate) struct HoldingMarshalReporter {
    pub(crate) held:
        std::sync::Arc<std::sync::Mutex<Vec<(u64, commonware_utils::acknowledgement::Exact)>>>,
}

impl commonware_consensus::Reporter for HoldingMarshalReporter {
    type Activity = crate::marshal_types::MarshalUpdate;

    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        use commonware_utils::Acknowledgement as _;
        if let commonware_consensus::marshal::Update::Block(block, ack) = activity {
            if block.inner().number() == 0 {
                ack.acknowledge();
            } else {
                self.held
                    .lock()
                    .unwrap()
                    .push((block.inner().number(), ack));
            }
        }
        commonware_actor::Feedback::Ok
    }
}

/// Recording marshal resolver capturing every `Key::Finalized { height }` hint dispatched by Marshal.
#[derive(Clone, Default)]
pub(crate) struct RecordingHintResolver {
    pub(crate) hinted_heights: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
}

impl RecordingHintResolver {
    fn record(
        &self,
        fetch: commonware_resolver::Fetch<
            commonware_consensus::marshal::resolver::handler::Key<crate::digest::Digest>,
            commonware_consensus::marshal::resolver::handler::Annotation,
        >,
    ) {
        if let commonware_consensus::marshal::resolver::handler::Key::Finalized { height } =
            fetch.key
        {
            self.hinted_heights.lock().unwrap().push(height.get());
        }
    }
}

impl commonware_resolver::Resolver for RecordingHintResolver {
    type Key = commonware_consensus::marshal::resolver::handler::Key<crate::digest::Digest>;
    type Subscriber = commonware_consensus::marshal::resolver::handler::Annotation;

    fn fetch<F>(&mut self, key: F) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        self.record(key.into());
        commonware_actor::Feedback::Ok
    }

    fn fetch_all<F>(&mut self, keys: impl IntoIterator<Item = F>) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        for key in keys {
            self.record(key.into());
        }
        commonware_actor::Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> commonware_actor::Feedback {
        commonware_actor::Feedback::Ok
    }
}

impl commonware_resolver::TargetedResolver for RecordingHintResolver {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        fetch: impl Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: commonware_utils::vec::NonEmptyVec<Self::PublicKey>,
    ) -> commonware_actor::Feedback {
        self.record(fetch.into());
        commonware_actor::Feedback::Ok
    }

    fn fetch_all_targeted<F>(
        &mut self,
        keys: impl IntoIterator<Item = (F, commonware_utils::vec::NonEmptyVec<Self::PublicKey>)>,
    ) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        for (key, _) in keys {
            self.record(key.into());
        }
        commonware_actor::Feedback::Ok
    }
}

pub(crate) struct ProcessedAbsentMarshalFixture {
    pub(crate) mailbox: crate::marshal_types::MarshalMailbox,
    pub(crate) held:
        std::sync::Arc<std::sync::Mutex<Vec<(u64, commonware_utils::acknowledgement::Exact)>>>,
    pub(crate) hinted_heights: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    pub(crate) _handler_keepalive:
        commonware_consensus::marshal::resolver::handler::Handler<crate::digest::Digest>,
    pub(crate) _actor_handle: commonware_runtime::Handle<()>,
}

/// Boots a deterministic Marshal positioned at `Processed::Absent(floor_height - 1)` by certifying
/// and setting a floor at `floor_height` while holding its block acknowledgement.
pub(crate) async fn start_processed_absent_marshal(
    context: &commonware_runtime::deterministic::Context,
    partition_prefix: &str,
    floor_height: u64,
) -> ProcessedAbsentMarshalFixture {
    use commonware_consensus::{
        marshal::{self, core::Processed, resolver::handler},
        simplex::types::{Finalization, Finalize, Proposal},
        types::{Epoch, Height, Round, View, ViewDelta},
    };
    use commonware_cryptography::{
        bls12381::{self, primitives::variant::MinSig},
        Signer as _,
    };
    use commonware_parallel::Sequential;
    use commonware_runtime::Supervisor as _;
    use commonware_utils::{
        ordered::{Quorum as _, Set},
        TryCollect as _,
    };
    use reth_ethereum::{primitives::SealedBlock, Block};
    use std::num::NonZeroU16;
    use std::sync::{Arc, Mutex};

    let keys: Vec<bls12381::PrivateKey> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let dkg = crate::bls::bootstrap_dkg(3).unwrap();
    let signers: Vec<crate::hybrid::HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let idx = participants.index(&key.public_key()).unwrap();
            crate::hybrid::HybridScheme::signer(
                &crate::config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[idx.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();
    let verifier = crate::hybrid::HybridScheme::<MinSig>::verifier(
        &crate::config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .unwrap();
    let provider = crate::hybrid::HybridSchemeProvider::new();
    assert!(provider.register(Epoch::new(0), verifier.clone()));

    let mut raw = Block::default();
    raw.header.number = floor_height;
    raw.header.extra_data = alloy_primitives::Bytes::from(vec![floor_height as u8]);
    let floor_block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        raw.map_header(outbe_primitives::OutbeHeader::new),
    ));
    let round = Round::new(Epoch::new(0), View::new(floor_height));
    let proposal = Proposal::new(round, View::new(floor_height - 1), floor_block.digest());
    let votes: Vec<_> = signers
        .iter()
        .map(|s| Finalize::sign(s, proposal.clone()).unwrap())
        .collect();
    let finalization = Finalization::from_finalizes(
        &verifier,
        commonware_utils::iter::NonEmpty::try_new(votes.iter()).unwrap(),
        &Sequential,
    )
    .unwrap();

    let cache = CacheRef::new(
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let items = NonZeroU64::new(10).unwrap();
    let buffer = NonZeroUsize::new(1024).unwrap();
    let (certificates, blocks) = MarshalArchiveFixture {
        partition_prefix,
        page_cache: &cache,
        items_per_section: items,
        replay_buffer: buffer,
        write_buffer: buffer,
    }
    .open(context)
    .await
    .unwrap();
    let genesis = Block::default().map_header(outbe_primitives::OutbeHeader::new);
    let (actor, mailbox, _) = marshal::core::Actor::init(
        context.child("marshal"),
        certificates,
        blocks,
        marshal::Config {
            provider,
            epocher: crate::follow::FollowerEpocher::new(100, 10),
            start: marshal::Start::Genesis(crate::test_fixtures::digested_block(
                crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(genesis)),
            )),
            partition_prefix: partition_prefix.into(),
            mailbox_size: NonZeroUsize::new(32).unwrap(),
            view_retention: ViewDelta::new(100),
            prunable_items_per_section: items,
            page_cache: cache,
            replay_buffer: buffer,
            key_write_buffer: buffer,
            value_write_buffer: buffer,
            block_codec_config: (),
            max_repair: NonZeroUsize::new(10).unwrap(),
            max_pending_acks: NonZeroUsize::new(1).unwrap(),
            strategy: Sequential,
        },
    )
    .await;

    let held = Arc::new(Mutex::new(Vec::new()));
    let resolver = RecordingHintResolver::default();
    let hinted_heights = Arc::clone(&resolver.hinted_heights);
    let (receiver, handler_keepalive) =
        handler::init(context.child("handler"), NonZeroUsize::new(32).unwrap());
    let actor_handle = actor.start_unbuffered(
        HoldingMarshalReporter {
            held: Arc::clone(&held),
        },
        (receiver, resolver),
    );

    wait_until_bounded(context, 200, "Processed::Block(0)", || async {
        mailbox.get_processed().await == Some(Processed::Block(Height::zero()))
    })
    .await;
    assert!(
        mailbox
            .verified(
                round,
                commonware_cryptography::Digested::precomputed(
                    floor_block.clone(),
                    floor_block.digest(),
                ),
            )
            .await
    );
    mailbox.set_floor(finalization);
    let absent_height = Height::new(floor_height - 1);
    wait_until_bounded(context, 200, "Processed::Absent(floor - 1)", || async {
        mailbox.get_processed().await == Some(Processed::Absent(absent_height))
    })
    .await;

    ProcessedAbsentMarshalFixture {
        mailbox,
        held,
        hinted_heights,
        _handler_keepalive: handler_keepalive,
        _actor_handle: actor_handle,
    }
}
