use super::*;

// Unique partition prefix per backfill-test marshal so the immutable
// archives never collide between concurrent test runs.
static BACKFILL_MARSHAL_ID: AtomicU64 = AtomicU64::new(0);

/// Block-availability buffer that never has any block: every lookup misses
/// and every subscription stays pending. Mirrors `EmptyMarshalBuffer` in
/// `application/handler_tests.rs`. Combined with `NoopResolver` (which never
/// fetches), an empty archive makes `marshal.get_block(h)` resolve `None`.
#[derive(Clone, Default)]
struct EmptyBackfillBuffer;

impl commonware_consensus::marshal::core::Buffer<crate::marshal_types::Variant>
    for EmptyBackfillBuffer
{
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    async fn find_by_digest(&self, _digest: Digest) -> Option<Arc<ConsensusBlock>> {
        None
    }

    async fn find_by_commitment(&self, _commitment: Digest) -> Option<Arc<ConsensusBlock>> {
        None
    }

    fn subscribe_by_digest(
        &self,
        _digest: Digest,
    ) -> Option<commonware_utils::channel::oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (_tx, rx) = commonware_utils::channel::oneshot::channel();
        Some(rx)
    }

    fn subscribe_by_commitment(
        &self,
        _commitment: Digest,
    ) -> Option<commonware_utils::channel::oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (_tx, rx) = commonware_utils::channel::oneshot::channel();
        Some(rx)
    }

    fn retire(&self, _update: commonware_consensus::marshal::core::Retirement<Digest>) {}

    fn send(
        &self,
        _round: commonware_consensus::types::Round,
        _block: Arc<ConsensusBlock>,
        _recipients: commonware_p2p::Recipients<Self::PublicKey>,
    ) {
    }
}

/// Reporter that acknowledges delivered blocks. Mirrors the
/// `AckingMarshalReporter` used by the other marshal harnesses.
#[derive(Clone, Default)]
struct AckingBackfillReporter;

impl commonware_consensus::Reporter for AckingBackfillReporter {
    type Activity = commonware_consensus::marshal::Update<
        ConsensusBlock,
        commonware_utils::acknowledgement::Exact,
    >;

    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        if let commonware_consensus::marshal::Update::Block(_, ack) = activity {
            ack.acknowledge();
        }
        commonware_actor::Feedback::Ok
    }
}

/// Resolver that never fetches anything. Mirrors the `NoopResolver` used by
/// the other marshal harnesses; required because `get_block` is local-only
/// (it never triggers a network fetch) and the resolver only exists so the
/// marshal actor can start.
#[derive(Clone, Default)]
struct NoopBackfillResolver;

impl commonware_resolver::Resolver for NoopBackfillResolver {
    type Key = commonware_consensus::marshal::resolver::handler::Key<Digest>;
    type Subscriber = commonware_consensus::marshal::resolver::handler::Annotation;

    fn fetch<F>(&mut self, _key: F) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        commonware_actor::Feedback::Ok
    }

    fn fetch_all<F>(&mut self, _keys: Vec<F>) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        commonware_actor::Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> commonware_actor::Feedback {
        commonware_actor::Feedback::Ok
    }
}

impl commonware_resolver::TargetedResolver for NoopBackfillResolver {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        _fetch: impl Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: commonware_utils::vec::NonEmptyVec<Self::PublicKey>,
    ) -> commonware_actor::Feedback {
        commonware_actor::Feedback::Ok
    }

    fn fetch_all_targeted<F>(
        &mut self,
        _keys: Vec<(F, commonware_utils::vec::NonEmptyVec<Self::PublicKey>)>,
    ) -> commonware_actor::Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        commonware_actor::Feedback::Ok
    }
}

/// Build genesis block (height 0) for the marshal `Start::Genesis` anchor.
fn backfill_genesis_block() -> ConsensusBlock {
    executor_test_block(0, 0x00)
}

/// Start an EMPTY marshal actor (no blocks in the archive, no-op resolver,
/// always-missing buffer) on the tokio runtime and return its mailbox.
///
/// `get_block(h)` on this mailbox resolves `None` for every height, because
/// the immutable archive is empty and `get_block` is a local-only lookup
/// (it never asks the network). The actor handle and the resolver handler
/// keepalive are returned so the caller keeps them alive for the test.
async fn start_empty_marshal(
    context: commonware_runtime::deterministic::Context,
) -> (
    crate::marshal_types::MarshalMailbox,
    commonware_consensus::marshal::resolver::handler::Handler<Digest>,
    commonware_runtime::Handle<()>,
) {
    use commonware_cryptography::{bls12381::primitives::variant::MinSig, certificate::Verifier};
    use commonware_runtime::buffer::paged::CacheRef;
    use commonware_storage::archive::immutable;
    use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

    use crate::hybrid::{HybridScheme, HybridSchemeProvider};

    let page_cache = CacheRef::from_pooler(
        &context,
        NonZeroU16::new(1024).expect("non-zero page size"),
        NonZeroUsize::new(10).expect("non-zero cache size"),
    );
    let test_id = BACKFILL_MARSHAL_ID.fetch_add(1, AtomicOrdering::SeqCst);
    let partition_prefix = format!("executor-backfill-{test_id}");
    let items_per_section = NonZeroU64::new(10).expect("non-zero items per section");
    let replay_buffer = NonZeroUsize::new(1024).expect("non-zero replay buffer");
    let write_buffer = NonZeroUsize::new(1024).expect("non-zero write buffer");

    let finalizations_archive = immutable::Archive::init(
        context.child("marshal_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: 64,
            freezer_table_resize_frequency: 10,
            freezer_table_resize_chunk_size: 10,
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
    .expect("finalizations archive should initialize");

    let blocks_archive = immutable::Archive::init(
        context.child("marshal_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: 64,
            freezer_table_resize_frequency: 10,
            freezer_table_resize_chunk_size: 10,
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
    .expect("blocks archive should initialize");

    let (actor, mailbox, _) = commonware_consensus::marshal::core::Actor::init(
        context.child("marshal"),
        finalizations_archive,
        blocks_archive,
        commonware_consensus::marshal::Config {
            provider: HybridSchemeProvider::<MinSig>::new(),
            epocher: commonware_consensus::types::FixedEpocher::new(
                NonZeroU64::new(10_000).expect("non-zero epoch"),
            ),
            start: commonware_consensus::marshal::Start::Genesis(backfill_genesis_block()),
            partition_prefix,
            mailbox_size: NonZeroUsize::new(32).expect("non-zero mailbox size"),
            view_retention: commonware_consensus::types::ViewDelta::new(10_000),
            prunable_items_per_section: items_per_section,
            page_cache,
            replay_buffer,
            key_write_buffer: write_buffer,
            value_write_buffer: write_buffer,
            block_codec_config: (),
            max_repair: NonZeroUsize::new(10).expect("non-zero max repair"),
            max_pending_acks: NonZeroUsize::new(1).expect("non-zero pending acks"),
            strategy: commonware_parallel::Sequential,
        },
    )
    .await;

    // The resolver receiver/handler pair: the marshal actor consumes the
    // receiver; the `Handler` is the keepalive (dropping it shuts the actor
    // down). The actor never fetches because `get_block` is local-only.
    let (resolver_rx, resolver_handler) =
        commonware_consensus::marshal::resolver::handler::init::<Digest>(
            context.child("resolver_handler"),
            NonZeroUsize::new(16).expect("non-zero resolver mailbox size"),
        );
    let handle = actor.start(
        AckingBackfillReporter,
        EmptyBackfillBuffer,
        (resolver_rx, NoopBackfillResolver),
    );
    (mailbox, resolver_handler, handle)
}

// TC-2 regression (finding TC-2 / F1 fix): the executor STARTUP BACKFILL must
// FAIL FAST when marshal cannot serve a finalized block at or below its own
// reported finalized height. `run()` walks `(execution_height,
// last_consensus_finalized]` calling `marshal.get_block(h)`; an empty marshal
// returns `None` for height 1, which means marshal's archive is inconsistent
// (claims finalized to N but cannot serve M <= N). The F1 fix makes that
// branch `error! + return Err(...)`. If it were reverted to the old
// `warn! + skip`, the backfill loop would fall through every height and then
// enter the infinite `run_live_loop`, so `run().await` would NOT return an
// `Err` (this test would hang on the wrapping timeout and then fail the
// "must return Err" assertion) - i.e. this test genuinely guards the fix.
#[test]
fn run_backfill_fails_fast_when_marshal_missing_finalized_block() {
    // The `Runner::timed` wedge guard replaces the previous outer
    // 20s wall-clock timeout safety net: `run()` must return promptly via
    // the backfill fail-fast branch. A hang here means the missing-block branch
    // fell through to `run_live_loop` (i.e. the F1 fix was reverted to
    // warn! + skip); the wedge guard aborts the test in that case.
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
        |context| async move {
            let genesis = B256::repeat_byte(0x01);
            // Dummy engine: branch (A) returns before any engine call, so the
            // receiver is simply never read.
            let (engine_tx, _engine_rx) = tokio::sync::mpsc::unbounded_channel();
            let engine = ConsensusEngineHandle::new(engine_tx);

            let (_projection_publisher, projection_readiness) = ready_projection(
                genesis,
                ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: genesis,
                },
            );

            // Executor at finalized height 0 (fresh bootstrap from genesis).
            let (actor, _mailbox) = super::ExecutorActor::new(
                context.child("exec"),
                engine,
                genesis,
                0,
                genesis,
                projection_readiness,
                None,
            );

            // Empty marshal: get_block(h) -> None for every height.
            let (marshal_mailbox, _resolver_keepalive, _marshal_handle) =
                start_empty_marshal(context.child("marshal_node")).await;

            // Consensus reports finalized height 3, execution is at 0, so the
            // backfill range is heights 1..=3. Height 1 misses -> fail fast.
            let run_result = actor.run(marshal_mailbox, Height::new(3)).await;

            assert!(
                run_result.is_err(),
                "an empty marshal that cannot serve a finalized block at/below its \
                 reported finalized height must make run() fail fast, not silently \
                 skip and continue; got {run_result:?}"
            );
            let message = format!("{:#}", run_result.expect_err("checked is_err above"));
            assert!(
                message.contains("missing finalized block"),
                "backfill fail-fast error must identify the missing finalized block; \
                 got: {message}"
            );
        },
    );
}
