use super::*;
use outbe_consensus::test_harness::NoopMarshalResolver;

static STACK_MARSHAL_TEST_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
struct EmptyMarshalBuffer {
    pending_digest_subscribers: Arc<StdMutex<Vec<oneshot::Sender<Arc<ConsensusBlock>>>>>,
    pending_commitment_subscribers: Arc<StdMutex<Vec<oneshot::Sender<Arc<ConsensusBlock>>>>>,
}

impl Buffer<outbe_consensus::marshal_types::Variant> for EmptyMarshalBuffer {
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    async fn find_by_digest(
        &self,
        _digest: outbe_consensus::digest::Digest,
    ) -> Option<Arc<ConsensusBlock>> {
        None
    }

    async fn find_by_commitment(
        &self,
        _commitment: outbe_consensus::digest::Digest,
    ) -> Option<Arc<ConsensusBlock>> {
        None
    }

    // `subscribe_by_*` are now SYNC and return `Option<oneshot::Receiver<..>>`.
    // We retain the pending sender (so the receiver never resolves) and hand
    // back `Some(rx)`, preserving the "block is never available" semantics this
    // empty buffer represents.
    fn subscribe_by_digest(
        &self,
        _digest: outbe_consensus::digest::Digest,
    ) -> Option<oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (tx, rx) = oneshot::channel();
        self.pending_digest_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    fn subscribe_by_commitment(
        &self,
        _commitment: outbe_consensus::digest::Digest,
    ) -> Option<oneshot::Receiver<Arc<ConsensusBlock>>> {
        let (tx, rx) = oneshot::channel();
        self.pending_commitment_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    fn retire(&self, _update: marshal::core::Retirement<outbe_consensus::digest::Digest>) {}

    fn send(
        &self,
        _round: Round,
        _block: Arc<ConsensusBlock>,
        _recipients: Recipients<Self::PublicKey>,
    ) {
    }
}

#[derive(Clone, Default)]
struct AckingMarshalReporter;

impl Reporter for AckingMarshalReporter {
    type Activity = Update<ConsensusBlock, commonware_utils::acknowledgement::Exact>;

    // `report` is now SYNC and returns `Feedback` (commonware 2026.5.0). The
    // body is unchanged work (acknowledge delivered blocks); we always return
    // `Feedback::Ok` because this test reporter has no downstream mailbox that
    // can close.
    fn report(&mut self, activity: Self::Activity) -> Feedback {
        if let Update::Block(_, ack) = activity {
            ack.acknowledge();
        }
        Feedback::Ok
    }
}

pub(super) async fn start_recovery_marshal(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
) {
    start_recovery_marshal_with_reporter(context, provider, AckingMarshalReporter).await
}

pub(super) async fn start_recovery_marshal_with_reporter<R>(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
    reporter: R,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
)
where
    R: Reporter<Activity = outbe_consensus::marshal_types::MarshalUpdate> + Send + 'static,
{
    let test_id = STACK_MARSHAL_TEST_ID.fetch_add(1, Ordering::SeqCst);
    start_recovery_marshal_in_partition(
        context,
        provider,
        reporter,
        format!("stack-finalized-round-recovery-{test_id}"),
        recovery_block(0),
    )
    .await
}

/// Copy/reopen fixtures use the same native partition names in different runtime roots.
pub(super) async fn start_recovery_marshal_in_partition<R>(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
    reporter: R,
    partition_prefix: String,
    genesis: ConsensusBlock,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
)
where
    R: Reporter<Activity = outbe_consensus::marshal_types::MarshalUpdate> + Send + 'static,
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
            epocher: FixedEpocher::new(NonZeroU64::new(10_000).unwrap()),
            // 2026.5.0: the floor/genesis anchor is now an explicit `Start`.
            // A fresh epoch starts from the height-0 genesis block (the actor
            // asserts the anchor height is zero).
            start: Start::Genesis(genesis),
            partition_prefix,
            // `mailbox_size` is now `NonZeroUsize`.
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

    // 2026.5.0: the resolver handoff changed - the marshal actor takes
    // `(handler::Receiver<Commitment>, R)` where `R: TargetedResolver`. The
    // receiver/handler pair is produced by `handler::init`; the `Handler` is
    // returned as the keepalive (dropping it closes the receiver and shuts the
    // actor's run loop down). The old `mpsc::Sender<handler::Message>` type is
    // now private and cannot be named or constructed by tests.
    let (resolver_rx, resolver_handler) = handler::init::<outbe_consensus::digest::Digest>(
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

/// Independent unbound-validator configuration for signer and restart scenarios.
pub(super) fn validator_signer_args(
    signing_key: std::path::PathBuf,
    evm_key: std::path::PathBuf,
) -> crate::args::ConsensusArgs {
    crate::args::ConsensusArgs {
        is_validator: true,
        signing_key: Some(signing_key),
        validator_evm_key: Some(evm_key),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: std::net::SocketAddr::from(([127, 0, 0, 1], 30400)),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_session_mode: crate::args::TeeSessionMode::PolicyDefault,
        tee_bootstrap_timeout_secs: 60,
        tee_canary_interval_secs: 30,
        tee_canary_failure_threshold: 3,
        txpool_pending_staleness_secs: 600,
        radicle_control_socket: None,
        radicle_status_address: None,
        upstream: None,
        upstream_nocertify: false,
        projection_storage_config: Some("/tmp/offchain-storage.toml".into()),
    }
}
