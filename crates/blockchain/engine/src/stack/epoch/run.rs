use super::super::*;

pub(in crate::stack) fn radicle_channel_config() -> (u64, u32) {
    (
        config::RADICLE_ENDPOINT_CHANNEL,
        config::RADICLE_ENDPOINT_CHANNEL_QUOTA,
    )
}

/// Muxer mailbox size for sub-channel buffering.
const MUXER_MAILBOX: usize = 1024;

/// epoch restart precondition: bounded wait for the finalization
/// view to expose the continuity anchor before launching the new-epoch
/// Simplex engine. Without this, `Automaton::genesis(epoch > 0)` could be
/// queried before the FinalizationActor publishes the boundary block's
/// anchor into `FinalizationView`, and Simplex would lock its `parent_view = 0`
/// to `B256::ZERO` permanently.
const EPOCH_RESTART_ANCHOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const EPOCH_RESTART_ANCHOR_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

pub fn run_consensus_stack<E>(
    ctx: E,
    args: ConsensusArgs,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    services: ConsensusStackServices,
) -> impl Future<Output = Result<()>> + Send
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let application = services.application_drain.clone();
    async move {
        // Do not spawn the fallible body in a child supervision task: completing
        // that task would abort its network before the application can drain.
        application
            .finish(run_consensus_stack_inner(ctx, args, node, bridge, services))
            .await
    }
}

async fn run_consensus_stack_inner<E>(
    ctx: E,
    args: ConsensusArgs,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    services: ConsensusStackServices,
) -> Result<()>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let ConsensusStackServices {
        application_drain,
        follower_shutdown,
        projection_readiness,
        ocomp_readiness,
        retained_tribute_writer,
        projection_retention_fence,
        retention_selector,
        finalized_ce_committer,
        ce_startup_recovery,
        radicle_status,
        radicle_endpoint,
    } = services;
    let mut radicle_updates = radicle_status.subscribe();

    // Validate network-scoped flags before any mode-specific early return. A
    // follower must fail closed too, even when it does not currently consume
    // these options.
    let chain_id = node.chain_spec().chain().id();
    let ocomp_fork_install =
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(node.chain_spec().as_ref())?;
    let ocomp_lifecycle_activation =
        OcompLifecycleActivation::at_block(ocomp_fork_install.activation_height);
    let ocomp_install_hash = Some(ocomp_fork_install.install_hash(&poc_schema_limits())?);
    validate_testnet_only_flags(
        args.trust_el_head,
        args.testnet_unix_time_offset_secs,
        chain_id,
    )?;

    // Follower mode: cold-sync finalized blocks from an upstream node and verify
    // them against the trusted network identity, WITHOUT running the consensus
    // engine. Short-circuits before any validator material is loaded.
    if let Some(upstream) = args.upstream.clone() {
        return run_follow_stack(
            ctx,
            args,
            node,
            bridge,
            upstream,
            projection_readiness,
            ocomp_readiness,
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector,
            finalized_ce_committer,
            ce_startup_recovery,
            follower_shutdown
                .ok_or_else(|| eyre::eyre!("follower pre-stop handshake is not installed"))?,
        )
        .await;
    }

    // -- 1. Load signing key ---------------------------------------------
    let signing_key_path = args
        .signing_key
        .as_ref()
        .ok_or_else(|| eyre::eyre!("--consensus.signing-key is required"))?;
    let key_backend = args.key_backend().wrap_err("invalid BLS key backend")?;
    let signing_key = validators::load_signing_key(signing_key_path, &key_backend)
        .wrap_err("failed to load signing key")?;

    // -- 2. Load validator set -------------------------------------------
    // Chain state is the only runtime source of validator membership. For a
    // fresh network this is the genesis ValidatorSet storage; for restart/join
    // this is the synced canonical state.
    let initial_peer_height = node
        .provider
        .last_block_number()
        .wrap_err("failed to read startup P2P admission height")?;
    let initial_peer_hash = node
        .provider
        .block_hash(initial_peer_height)?
        .ok_or_else(|| eyre::eyre!("startup P2P admission block is missing"))?;
    let mut validator_set =
        validators::read_consensus_validators_at_block(&node.provider, initial_peer_hash)
            .wrap_err("failed to load consensus validator set at startup")?;

    info!(
        count = validator_set.public_keys.len(),
        "loaded validator set"
    );

    // -- 3. Set up P2P network -------------------------------------------
    let p2p_namespace = ocomp_p2p_namespace(ocomp_install_hash);
    // Cover the full registered validator set plus a local non-validator identity.
    let max_peers_per_set =
        NonZeroUsize::new(outbe_consensus::bls::MAX_VALIDATORS as usize + 1).unwrap();
    let network_cfg = if args.use_local_defaults {
        lookup::Config::local(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            max_peers_per_set,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    } else {
        lookup::Config::recommended(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            max_peers_per_set,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    };

    let (mut network, mut oracle) = lookup::Network::new(ctx.child("network"), network_cfg);

    // Register Simplex consensus channels (will be wrapped in Muxers).
    let votes = network.register(config::VOTES_CHANNEL, Quota::per_second(NZU32!(128)));
    let certificates =
        network.register(config::CERTIFICATES_CHANNEL, Quota::per_second(NZU32!(128)));
    let resolver = network.register(config::RESOLVER_CHANNEL, Quota::per_second(NZU32!(64)));

    // Register broadcast channel for block dissemination (buffered engine).
    let broadcast_channel =
        network.register(config::BROADCAST_CHANNEL, Quota::per_second(NZU32!(32)));

    // Register marshal resolver channel for on-demand block backfill.
    let marshal_channel = network.register(config::MARSHAL_CHANNEL, Quota::per_second(NZU32!(64)));

    // Register DKG ceremony channel (muxed by reshare round).
    let dkg_channel = network.register(config::DKG_CHANNEL, Quota::per_second(NZU32!(128)));

    // Register the one-time TEE bootstrap channel (only when a TEE enclave
    // sidecar is configured). Used once at startup, like the DKG, to coordinate
    // the committee's enclave registrations + EVM signatures into the block-1
    // `TeeBootstrap` payload. Registered before `network.start()`.
    let mut tee_bootstrap_channel = args
        .tee_enclave_socket
        .as_ref()
        .map(|_| network.register(config::TEE_BOOTSTRAP_CHANNEL, Quota::per_second(NZU32!(64))));

    // Register the one-time TEE DKG channel (only when a TEE enclave sidecar is
    // configured). Carries the enclave identity exchange + dealer/player gossip +
    // offer-key partial-signature round that derives the shared tribute offer key
    // at startup. Registered before `network.start()`.
    let mut tee_dkg_channel = args
        .tee_enclave_socket
        .as_ref()
        .map(|_| network.register(config::TEE_DKG_CHANNEL, Quota::per_second(NZU32!(128))));

    let radicle_channel = radicle_endpoint.as_ref().map(|_| {
        let (channel, quota) = radicle_channel_config();
        network.register(
            channel,
            Quota::per_second(NonZeroU32::new(quota).expect("Radicle quota is non-zero")),
        )
    });

    // Parse consensus peers: `<hex_pubkey>@<host:port>` -> (PublicKey, SocketAddr).
    let bootnode_map = parse_consensus_peers(&args.consensus_peers)?;

    if !bootnode_map.is_empty() {
        info!(count = bootnode_map.len(), "parsed bootnode entries");
    }

    // Build peer set from validator config + bootnodes.
    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    let admitted_set =
        validators::read_admitted_non_consensus_at_block(&node.provider, initial_peer_hash)
            .wrap_err("failed to read startup non-consensus P2P admission")?;
    let initial_peers = commonware_p2p::AddressableTrackedPeers::new(
        peer_map,
        build_peer_map(&admitted_set, &bootnode_map),
    );
    let resolved_count = initial_peers.primary.len();
    eyre::ensure!(
        oracle.track(0, initial_peers.clone()) == commonware_actor::Feedback::Ok,
        "P2P oracle closed during startup admission"
    );
    info!(
        total = validator_set.public_keys.len(),
        resolved = resolved_count,
        bootnodes = bootnode_map.len(),
        "P2P peer set registered with oracle"
    );

    // -- 4. Start P2P network (needed before DKG can run) ---------------
    let mut network_handle = network.start();
    info!("P2P network started");

    if let (Some((endpoint, local, owner)), Some((sender, receiver))) =
        (radicle_endpoint, radicle_channel)
    {
        let signer = signing_key.clone();
        if !owner.start(async move {
            let result = endpoint.run(sender, receiver, signer, local).await;
            if let Err(error) = &result {
                tracing::warn!(%error, "Radicle endpoint actor stopped");
            }
            result
        })? {
            return Ok(());
        }
    }

    // -- 5. Create Muxers from physical channels ------------------------
    // Consensus channels are muxed by epoch - each engine restart
    // gets fresh sub-channels, preventing message interference.
    let (vote_muxer, mut vote_mux) =
        Muxer::new(ctx.child("vote_mux"), votes.0, votes.1, MUXER_MAILBOX);
    vote_muxer.start();

    let (cert_muxer, mut cert_mux) = Muxer::new(
        ctx.child("cert_mux"),
        certificates.0,
        certificates.1,
        MUXER_MAILBOX,
    );
    cert_muxer.start();

    let (res_muxer, mut res_mux) =
        Muxer::new(ctx.child("res_mux"), resolver.0, resolver.1, MUXER_MAILBOX);
    res_muxer.start();

    // Stash for sub-channels pre-registered at DKG completion.
    // The activation handler pre-registers vote/cert/res sub-channels for
    // the upcoming epoch as soon as DKG completes - well before the
    // boundary's planned activation height. By the time any peer fires
    // its activation handler, every honest node already has routes for
    // the new epoch's sub-channels and cannot drop early proposals/votes
    // because of an unregistered sub-channel (Mode-B race). The top of
    // the next `'epoch_loop` iteration consumes this stash via
    // `take_or_register_current`.
    let mut next_epoch_subchannels: Option<
        outbe_consensus::epoch_subchannels::EpochSubchannels<_, _>,
    > = None;
    // Same-epoch role replacement has a distinct stash so it cannot overwrite
    // channels already pre-registered for a future DKG epoch.
    let mut replacement_epoch_subchannels: Option<
        outbe_consensus::epoch_subchannels::EpochSubchannels<_, _>,
    > = None;

    // DKG channel muxed by reshare round.
    let (dkg_muxer, mut dkg_mux) = Muxer::new(
        ctx.child("dkg_mux"),
        dkg_channel.0,
        dkg_channel.1,
        MUXER_MAILBOX,
    );
    dkg_muxer.start();

    // R5.4: mux the TEE DKG + TEE bootstrap channels by round, mirroring `dkg_mux`,
    // so the startup ceremony (round 0) and a later epoch-boundary reshare (round N)
    // each get isolated sub-channels. `None` when no TEE enclave sidecar is set.
    let mut tee_dkg_mux = tee_dkg_channel.take().map(|ch| {
        let (muxer, handle) = Muxer::new(ctx.child("tee_dkg_mux"), ch.0, ch.1, MUXER_MAILBOX);
        muxer.start();
        handle
    });
    let mut tee_bootstrap_mux = tee_bootstrap_channel.take().map(|ch| {
        let (muxer, handle) = Muxer::new(ctx.child("tee_boot_mux"), ch.0, ch.1, MUXER_MAILBOX);
        muxer.start();
        handle
    });

    // R5.4: pre-register the round-0 TEE sub-channels EARLY (mirroring the
    // consensus `dkg_mux.register(0)` at startup) so every node has round 0 routed
    // well before the startup TEE DKG begins. Registering it lazily inside the
    // startup block races: a node can broadcast its identity before a peer has
    // registered round 0, and the mux drops the unrouted message -> the identity
    // exchange hangs. Reshare rounds (N>0) still register on demand at the boundary.
    let mut tee_dkg_round0 = match tee_dkg_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE DKG round 0: {e}"))?,
        ),
        None => None,
    };
    let mut tee_bootstrap_round0 = match tee_bootstrap_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE bootstrap round 0: {e}"))?,
        ),
        None => None,
    };
    info!("channel muxers started");

    // Startup chain-state sources must exist before threshold material selection:
    // DKG round 0 is allowed only when both execution and marshal prove genesis
    // formation. Local execution height 0 alone is not sufficient for a fresh
    // datadir joining an already-running network.
    let genesis_hash = genesis_hash(&node)?;
    let epoch_length_blocks = epoch_length_blocks_from_genesis(&node)?;
    let dkg_rotation_params = DkgRotationParams::from_genesis(&node, epoch_length_blocks);

    // -- 5b. Pre-compute page cache (shared across marshal + epochs) -----
    let page_cache = CacheRef::from_pooler(
        &ctx,
        nonzero_u16(4096, "page cache page size")?,
        nonzero_usize(config::PAGE_CACHE_SIZE / 4096, "PAGE_CACHE_SIZE / 4096")?,
    );

    // -- 5c. Initialize marshal actor before threshold material selection -
    //
    // Marshal init exposes persisted consensus finalized height. That height is
    // part of the genesis-formation proof; without it a crash-restart with
    // execution height 0 could incorrectly start DKG round 0.
    use commonware_consensus::marshal;
    use commonware_cryptography::{certificate::Verifier as CertVerifier, Signer as _};
    use commonware_storage::archive::immutable;

    let certificate_scheme_provider = HybridSchemeProvider::<MinSig>::new();
    let elector_config_provider = HybridElectorConfigProvider::<MinSig>::new();
    let committee_provider = CommitteeProvider::new();

    let partition_prefix = "outbe-marshal".to_string();

    let finalizations_archive = immutable::Archive::init(
        ctx.child("marshal_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: (),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize blocks archive")?;

    let epocher = commonware_consensus::types::FixedEpocher::new(nonzero_u64(
        u64::from(epoch_length_blocks),
        "epochLengthBlocks",
    )?);
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

    let marshal_genesis_anchor = genesis_consensus_block(&node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal::Config {
                provider: certificate_scheme_provider.clone(),
                epocher,
                start: marshal::Start::Genesis(marshal_genesis_anchor),
                partition_prefix: partition_prefix.clone(),
                mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
                view_retention: ViewDelta::new(view_retention_timeout),
                prunable_items_per_section: nonzero_u64(
                    config::PRUNABLE_ITEMS_PER_SECTION,
                    "PRUNABLE_ITEMS_PER_SECTION",
                )?,
                page_cache: page_cache.clone(),
                replay_buffer: nonzero_usize(
                    config::MARSHAL_REPLAY_BUFFER,
                    "MARSHAL_REPLAY_BUFFER",
                )?,
                key_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                value_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                block_codec_config: (),
                max_repair: nonzero_usize(config::MAX_REPAIR, "MAX_REPAIR")?,
                max_pending_acks: nonzero_usize(config::MAX_PENDING_ACKS, "MAX_PENDING_ACKS")?,
                strategy: commonware_parallel::Sequential,
            },
        )
        .await;

    // commonware 2026.5.0: `Actor::init` now returns `Option<Height>` - `None`
    // means no durable consensus finalization yet (fresh genesis). Map that to
    // height 0, preserving the prior non-optional `Height` semantics used by the
    // genesis-formation proof, crash-recovery detection, and executor start.
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt.height());

    info!(
        marshal_processed_height = last_consensus_finalized.get(),
        "marshal actor initialized; exact archive/Reth recovery reconciliation pending"
    );

    let local_consensus_key = signing_key.public_key();
    let startup_snapshot = resolve_startup_dkg_snapshot(
        ctx.child("startup_dkg_snapshot"),
        &node,
        &args,
        &key_backend,
        local_consensus_key.clone(),
        &validator_set,
        genesis_hash,
        dkg_rotation_params,
        last_consensus_finalized.get(),
    )
    .await?;
    let last_execution_height = startup_snapshot.last_execution_height;
    let last_execution_hash = startup_snapshot.last_execution_hash;
    let recovered_boundary = startup_snapshot.recovered_boundary;
    let recovered_pending_boundary = startup_snapshot.pending_boundary;
    let startup_dkg_context = startup_snapshot.context;

    // Determine founding versus existing identity before any DKG/live-join path.
    // The mandatory enclave client was installed by the node entrypoint before
    // Reth launch; this gate prevents threshold work and consensus startup from
    // treating the pre-DKG onboarding recipient as a permanent offer key.
    let verifier_join = args.signing_share.is_none()
        && args.public_polynomial.is_some()
        && args.dkg_output.is_some();
    let local_key_in_current_consensus_set = validator_set
        .public_keys
        .iter()
        .any(|key| key == &local_consensus_key);
    let on_chain_offer = validators::read_tee_offer_public_at_latest(&node.provider)
        .wrap_err("failed to read canonical offer key before threshold work")?;
    let resident_offer = outbe_tee::resident_offer_public_key_state_v1()
        .wrap_err("failed to read enclave offer-key readiness before threshold work")?;
    validate_offer_key_before_threshold_work(
        startup_dkg_context,
        local_key_in_current_consensus_set,
        verifier_join,
        on_chain_offer,
        resident_offer,
    )?;
    info!(
        founding = startup_dkg_mode(startup_dkg_context, local_key_in_current_consensus_set)
            == StartupDkgMode::InitialGenesisDkg
            && !verifier_join,
        offer_key_ready = resident_offer.is_some(),
        canonical_offer_key_present = !on_chain_offer.is_zero(),
        "permanent offer-key gate passed before threshold work"
    );

    // -- 6. Obtain threshold material ------------------------------------
    // For initial DKG, use subchannel 0 of the DKG mux.
    let (dkg_init_tx, dkg_init_rx) = dkg_mux
        .register(0)
        .await
        .map_err(|e| eyre::eyre!("failed to register initial DKG subchannel: {e}"))?;

    let recovered_shareless_output = recovered_boundary
        .as_ref()
        .map(|(_height, boundary)| decode_boundary_output(boundary))
        .transpose()?
        .filter(|output| output.players().position(&local_consensus_key).is_none());
    let threshold_material = if let Some(output) = recovered_shareless_output {
        info!(
        target: "outbe_engine::stack",
                   dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                   "local validator is absent from the finalized DKG boundary; restoring shareless verifier mode"
               );
        ThresholdMaterial::VerifierOnly {
            polynomial: output.public().clone(),
            last_dkg_output: Some(output),
        }
    } else {
        obtain_threshold_material(
            ctx.child("initial_dkg_material"),
            &args,
            &key_backend,
            signing_key.clone(),
            &validator_set,
            startup_dkg_context,
            dkg_init_tx,
            dkg_init_rx,
        )
        .await?
    };
    let (mut signing_share, mut polynomial, mut last_dkg_output, bootstrap_from_live_dkg) =
        match threshold_material {
            ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            } => (
                Some(signing_share),
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            ),
            ThresholdMaterial::VerifierOnly {
                polynomial,
                last_dkg_output,
            } => (None, polynomial, last_dkg_output, false),
        };
    // Threshold material, not CLI file presence, owns consensus authority. A
    // recovered validator excluded from the current boundary is VerifierOnly even
    // when its original signing-share path is still configured on disk.
    let shareless_verifier = signing_share.is_none();

    // Verifier-join supplies public threshold material without a signing share.
    // Its local database can still be at height zero while it joins an already
    // running chain, so local height alone must never reproduce genesis DKG/OST3.
    let coordinate_genesis_bootstrap = should_coordinate_genesis_tee_bootstrap(
        startup_dkg_context,
        local_key_in_current_consensus_set,
        shareless_verifier,
    );

    // Block 1 carries `BoundaryOutcome` before `TeeBootstrap`. Only a proven
    // founding member builds that canonical boundary from the completed
    // consensus DKG and reuses its committee hash for OST3. The corresponding
    // snapshot does not and must not exist in provider state until block 1
    // executes.
    let mut genesis_dkg_boundary_artifact = if coordinate_genesis_bootstrap {
        let bootstrap_output = last_dkg_output.as_ref().ok_or_else(|| {
            eyre::eyre!(
                "fresh bootstrap requires full DKG output; public polynomial alone cannot build canonical boundary"
            )
        })?;
        Some(build_genesis_dkg_boundary_artifact(
            &validator_set,
            bootstrap_output,
            bootstrap_from_live_dkg,
        )?)
    } else {
        None
    };

    // -- 7. Build participant set (updated after each DKG reshare) -------
    // when recovering a finalized DKG boundary, reconstruct the scheme
    // against the committee the recovered threshold material belongs to (the DKG
    // output's players), NOT the latest on-chain set, which may have drifted
    // across a churn window. `select_recovery_participants` also fails fast if the
    // restored material does not match the recovered boundary. On a fresh chain or
    // when no boundary/output is recovered, fall back to the latest committed set
    // (the genesis committee on first start).
    let mut participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        match (recovered_boundary.as_ref(), last_dkg_output.as_ref()) {
            (Some((_, boundary)), Some(output)) => {
                select_recovery_participants(output.players(), boundary)?
            }
            _ => validator_set
                .public_keys
                .clone()
                .into_iter()
                .try_collect()
                .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?,
        };

    let reshare_target_validator_set = {
        let state = node
            .provider
            .latest()
            .wrap_err("failed to load latest state for EVM signer validation")?;
        validators::read_reshare_target_from_state(&state)
            .wrap_err("failed to load current reshare target for EVM signer validation")?
    };
    let recovered_committee_for_signer = recovered_boundary
        .as_ref()
        .map(|(_, boundary)| (&participants, boundary));
    let proposer_evm_address = validate_validator_evm_signer(
        &args,
        &signing_key,
        &validator_set,
        &reshare_target_validator_set,
        recovered_committee_for_signer,
        shareless_verifier,
    )?;

    // -- 7b. One-time TEE DKG + bootstrap coordination (startup, like the DKG) --
    // On a fresh chain (no executed blocks yet), this validator must run the TEE
    // enclave sidecar selected by the mandatory block-1 genesis policy:
    //   1. run the TEE DKG ceremony so the committee's enclaves collaboratively
    //      derive the shared tribute offer key (Seam F: a group threshold
    //      signature over a fixed message -> HKDF -> X25519; byte-identical on every
    //      honest node, secret resident in each enclave); then
    //   2. coordinate the committee's enclave registrations + EVM signatures into
    //      the block-1 `TeeBootstrap` payload - registering the DKG-derived offer
    //      key - and stash it in the bridge for the proposer to inject (slice 5.1).
    // `committee_snapshot_block` is the fixed block 1. The whole ceremony MUST
    // complete before block 1: it is wrapped in `--tee-bootstrap-timeout-secs` and
    // FAILS FAST (node halts via startup error) on timeout or error, rather than
    // proceeding into a permanently un-bootstrapped chain (no offer key on-chain =>
    // offers impossible). Local liveness only - not a consensus rule on imported
    // blocks. Missing local enclave or NodeHost identity is a startup error,
    // never a production fallback.
    let socket = args.tee_enclave_socket.clone().ok_or_else(|| {
        eyre::eyre!("mandatory TEE chain requires --tee-enclave-socket before consensus startup")
    })?;
    let tee_attestation =
        outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
            node.chain_spec().as_ref(),
        );
    let tee_activation = tee_attestation
        .activation()
        .map_err(|error| eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}"))?;
    let tee_policy = tee_activation
        .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
        .map_err(eyre::Report::msg)?
        .clone();
    let tee_session = args
        .tee_session_mode
        .resolve(tee_policy.attestation_mode)
        .map_err(eyre::Report::msg)?;
    {
        if coordinate_genesis_bootstrap {
            let my_validator = proposer_evm_address.ok_or_else(|| {
                eyre::eyre!("founding validator TEE bootstrap requires its EVM identity")
            })?;
            let n = participants.len();
            let tee_remote_peers: std::collections::BTreeSet<bls12381::PublicKey> = participants
                .iter()
                .filter(|peer| *peer != &local_consensus_key)
                .cloned()
                .collect();
            let dkg_remote_peers = tee_remote_peers.clone();
            let deadline = std::time::Duration::from_secs(args.tee_bootstrap_timeout_secs);

            let (dkg_sender, dkg_receiver) = tee_dkg_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE DKG P2P channel not registered"))?;
            let (tee_sender, tee_receiver) = tee_bootstrap_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE bootstrap P2P channel not registered"))?;
            let (evm_signer, participant_committee) =
                tee_bootstrap_setup(&args, &participants, &validator_set)?;
            let genesis_boundary = genesis_dkg_boundary_artifact.as_ref().ok_or_else(|| {
                eyre::eyre!("fresh OST3 bootstrap is missing its canonical genesis DKG boundary")
            })?;
            let committee_snapshot_hash = genesis_boundary.committee_set_hash;
            let committee: std::collections::BTreeSet<alloy_primitives::Address> = genesis_boundary
                .reshare
                .new_active_set
                .iter()
                .copied()
                .collect();
            ensure!(
                committee == participant_committee,
                "OST3 epoch-0 DKG boundary differs from the live genesis participants"
            );
            ensure!(
                committee.contains(&my_validator),
                "local validator is absent from the exact OST3 epoch-0 committee snapshot"
            );
            let requested_valid_until = node
                .chain_spec()
                .genesis
                .timestamp
                .checked_add(tee_policy.maximum_lease)
                .ok_or_else(|| eyre::eyre!("OST3 deterministic block-1 lease overflows u64"))?;
            let node_data_dir = node
                .config
                .datadir
                .clone()
                .resolve_datadir(node.chain_spec().chain())
                .data_dir()
                .to_path_buf();
            let endpoint = socket
                .to_str()
                .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?
                .to_owned();
            let reth_p2p_secret = node
                .config
                .network
                .secret_key(node.data_dir.p2p_secret())
                .wrap_err("failed to load persistent Reth P2P identity for OST3")?;
            let node_host_signing =
                k256::ecdsa::SigningKey::from_slice(reth_p2p_secret.secret_bytes().as_slice())
                    .map_err(|error| eyre::eyre!("invalid Reth P2P signing key: {error}"))?;
            let reth_p2p_public = node_host_signing
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))?;
            let node_id = outbe_primitives::tee_attestation_v1::NodeIdV1 { reth_p2p_public };

            // Step 1 (TEE DKG -> shared offer key) + Step 2 (bootstrap coordination ->
            // block-1 payload), under one deadline. Any error or timeout halts.
            // The deadline is measured on the consensus runtime `Clock` (the same
            // time source the deterministic test runtime can mock and advance), not
            // wall-clock - keeping startup-timeout behavior reproducible and free of a
            // direct async-runtime timer dependency in the consensus stack.
            // `Clock::timeout` requires a `Send + 'static` future; the `async move`
            // owns every capture, so the bound holds.
            // Owned `Clock` clone moved into the `'static` startup future so the TEE
            // DKG identity-exchange cadence runs on the consensus runtime clock, not
            // tokio's wall-clock (mockable under the deterministic test runtime).
            let dkg_clock = ctx.child("tee_dkg_clock");
            let bootstrap_clock = ctx.child("tee_bootstrap_clock");
            let payload = ctx
                .timeout(deadline, async move {
                    let (mut enclave, production_manifest) = match tee_session {
                        crate::args::ResolvedTeeSession::ProductionNodeHost => {
                            let production = outbe_tee::connect_committed_node_host_enclave(
                                &endpoint,
                                &node_data_dir,
                            )
                            .map_err(|error| {
                                eyre::eyre!("production NodeHost reconnect failed: {error}")
                            })?;
                            let manifest =
                                outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
                                    .map_err(|error| {
                                        eyre::eyre!(
                                            "committed NodeHost manifest load failed: {error}"
                                        )
                                    })?;
                            (
                                outbe_tee::RuntimeEnclaveClient::Production(production),
                                Some(manifest),
                            )
                        }
                        crate::args::ResolvedTeeSession::Development => {
                            let development = outbe_tee::EnclaveClient::connect_endpoint(&endpoint)
                                .map_err(|error| {
                                    eyre::eyre!(
                                        "GramineDirectDev enclave reconnect failed: {error}"
                                    )
                                })?;
                            (
                                outbe_tee::RuntimeEnclaveClient::Development(Box::new(development)),
                                None,
                            )
                        }
                    };
                    let (tribute_offer_public, tribute_offer_group_public_key) =
                        crate::tee_bootstrap::run_tee_dkg_at_startup(
                            &mut enclave,
                            dkg_clock,
                            n,
                            tee_policy.network_binding(),
                            0,
                            dkg_remote_peers,
                            dkg_sender,
                            dkg_receiver,
                        )
                        .await
                        .map_err(|e| eyre::eyre!("TEE DKG ceremony failed: {e}"))?;
                    info!(
                        tribute_offer_public = %B256::from(tribute_offer_public),
                        "TEE DKG complete - shared tribute offer key derived"
                    );
                    let local_submission =
                        crate::tee_bootstrap::build_local_tee_bootstrap_submission_v2(
                            &mut enclave,
                            production_manifest.as_ref(),
                            node_id,
                            &tee_policy,
                            requested_valid_until,
                            &evm_signer,
                            |hash| {
                                use k256::ecdsa::signature::hazmat::PrehashSigner as _;
                                let (signature, recovery): (
                                    k256::ecdsa::Signature,
                                    k256::ecdsa::RecoveryId,
                                ) = node_host_signing
                                    .sign_prehash(hash.as_slice())
                                    .map_err(|error| error.to_string())?;
                                let mut bytes = [0_u8; 65];
                                bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
                                bytes[64] = recovery.to_byte();
                                Ok(bytes)
                            },
                        )?;
                    let authority = outbe_primitives::tee_bootstrap_v2::TeeBootstrapAuthorityV2 {
                        policy: tee_policy,
                        committee_snapshot_hash,
                        committee_snapshot_block: 1,
                        key_epoch: 0,
                        tribute_offer_epoch: 0,
                        dkg_transcript_hash: B256::ZERO,
                        tribute_offer_public_key: B256::from(tribute_offer_public),
                        tribute_offer_group_public_key: Bytes::from(tribute_offer_group_public_key),
                    };
                    let payload = crate::tee_bootstrap::run_tee_bootstrap_v2_at_startup(
                        local_submission,
                        authority,
                        committee,
                        &evm_signer,
                        tee_remote_peers,
                        tee_sender,
                        tee_receiver,
                        bootstrap_clock,
                    )
                    .await
                    .map_err(|e| eyre::eyre!("TEE bootstrap coordination failed: {e}"))?;
                    Ok::<_, eyre::Report>(payload)
                })
                .await
                .map_err(|_| {
                    eyre::eyre!(
                        "TEE DKG + bootstrap did not complete within {}s \
                     (--tee-bootstrap-timeout-secs); halting before block 1",
                        args.tee_bootstrap_timeout_secs
                    )
                })??;

            info!(
                validators = payload.participants.len(),
                attestation_mode = ?payload.policy.attestation_mode,
                "mandatory OST3 bootstrap coordinated - payload ready for block 1"
            );
            bridge.set_pending_tee_bootstrap(payload);
        } else if shareless_verifier {
            // The permissionless V1 onboarding transaction must have installed
            // the permanent key before this process was launched. A shareless
            // validator certifies and replays the existing chain without threshold
            // authority and never reproduces genesis OST3. Its canonical EVM/BLS
            // identity may already be retained for the later DKG activation.
            let resident_offer = outbe_tee::resident_offer_public_key_v1().wrap_err(
                "verifier-join requires the permanent resident offer key before certified sync (no recovery or fallback)",
            )?;
            ensure!(
                !resident_offer.is_zero(),
                "verifier-join enclave has no permanent resident offer key; refusing certified sync (no recovery or fallback)"
            );
            info!(
                offer_public_key = %resident_offer,
                "verifier-join resident offer key present before certified sync"
            );
        } else {
            // A restarted active validator must already hold the exact resident
            // offer key committed on-chain. Startup never recovers or replaces a
            // lost key and never falls back to a pre-V1 delivery protocol.
            let _my_validator = proposer_evm_address.ok_or_else(|| {
                eyre::eyre!("active validator consensus startup requires its EVM identity")
            })?;
            let on_chain_offer = validators::read_tee_offer_public_at_latest(&node.provider)
                .wrap_err("failed to read the mandatory on-chain TEE offer key")?;
            ensure!(
                !on_chain_offer.is_zero(),
                "mandatory OST3 chain has no on-chain offer key after block 1"
            );
            let node_data_dir = node
                .config
                .datadir
                .clone()
                .resolve_datadir(node.chain_spec().chain())
                .data_dir()
                .to_path_buf();
            let endpoint = socket
                .to_str()
                .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
            let mut enclave = match tee_session {
                crate::args::ResolvedTeeSession::ProductionNodeHost => {
                    outbe_tee::RuntimeEnclaveClient::Production(
                        outbe_tee::connect_committed_node_host_enclave(endpoint, &node_data_dir)
                            .map_err(|error| {
                                eyre::eyre!("production NodeHost reconnect failed: {error}")
                            })?,
                    )
                }
                crate::args::ResolvedTeeSession::Development => {
                    outbe_tee::RuntimeEnclaveClient::Development(Box::new(
                        outbe_tee::EnclaveClient::connect_endpoint(endpoint).map_err(|error| {
                            eyre::eyre!("GramineDirectDev enclave reconnect failed: {error}")
                        })?,
                    ))
                }
            };
            let enclave_offer = crate::tee_bootstrap::query_enclave_offer_public(&mut enclave)?;
            ensure!(
                enclave_offer == on_chain_offer,
                "local enclave does not hold the chain offer key; refusing consensus startup (no recovery or fallback)"
            );
        }
    }

    // -- 8. Recover execution finalized state ------------------------------
    let active_boundary = recovered_boundary.clone();

    let recovered_boundary_artifact = active_boundary.as_ref().map(|(_, artifact)| artifact);
    let mut vrf_material_version = recovered_boundary_artifact
        .map(|artifact| artifact.vrf_material_version)
        .unwrap_or(0);
    // These strict checks (saved polynomial / DKG output must equal the recovered
    // finalized boundary) only matter for a SIGNER, which signs with its polynomial +
    // share. A share-less VERIFIER follows finality via the certificate's PARTICIPANT
    // set (not its polynomial), so its CLI `--public-polynomial`/`--dkg-output` may be
    // off (e.g. a TEE chain's runtime-derived genesis consensus polynomial differs
    // from the bootstrap file, or the chain has rotated past it) without affecting
    // sync - only its local VRF/leader view is degraded (process-local, non-fatal,
    // same as the post-rotation verifier-follower case). Enforcing these on a restarted
    // verifier would fatally crash an otherwise-healthy follower, so gate them to
    // signers; the verifier syncs and the running epoch loop advances it.
    if signing_share.is_some() {
        validate_recovered_vrf_material(&polynomial, recovered_boundary_artifact)?;
        if let (Some(output), Some(boundary)) = (&last_dkg_output, recovered_boundary_artifact) {
            let canonical_output = decode_boundary_output(boundary)
                .wrap_err("failed to decode recovered DKG boundary output")?;
            dkg_manager::assert_canonical_output(output, &canonical_output, "restart recovery")?;
        }
    } else if let Some(boundary) = recovered_boundary_artifact {
        // Verifier-follower with a recovered on-chain DKG boundary (e.g. a restart, or
        // a TEE chain whose runtime genesis consensus output differs from the bootstrap
        // CLI files): adopt the chain's CURRENT canonical DKG output as both the
        // polynomial and the reshare prev_output. The DKG reshare ceremony binds the
        // FULL previous output into its `info_hash` (not just the group key), so if the
        // verifier later becomes a frozen-target player it MUST present the committee's
        // current output as prev_output - its stale `--consensus.dkg-output` would yield
        // a divergent `info_hash`, the dealers' bundles get dropped, and the ceremony
        // times out (the node never gets a share). The genesis/boundary artifact carries
        // the full `Output`, so `decode_boundary_output` recovers exactly what the
        // committee holds. Finality still verifies via the participant set regardless.
        let canonical_output = decode_boundary_output(boundary)
            .wrap_err("failed to decode recovered DKG boundary output for verifier")?;
        polynomial = canonical_output.public().clone();
        last_dkg_output = Some(canonical_output);
    }
    let vrf_materials = VrfMaterialProvider::new(
        vrf_material_version,
        polynomial.clone(),
        signing_share.clone(),
    );
    bridge.set_local_threshold_share_present(signing_share.is_some());
    // The active boundary tuple height is the ACTIVATION ANCHOR (the height the
    // live committee anchored its rotation schedule on), NOT the commit height of
    // the artifact-carrying block: finalized BoundaryOutcome recovery normalizes
    // commit -> anchor (commit - 1, since the artifact rides the first new-epoch
    // block). A node-local pending snapshot is restored separately and never
    // changes this active anchor until its exact outgoing-finalized preannounce
    // authorizes the normal runtime activation path.
    let mut last_dkg_activation_height = active_boundary
        .as_ref()
        .map(|(height, _)| *height)
        .unwrap_or(last_execution_height);
    let mut dkg_cycle = recovered_boundary_artifact
        .map(|artifact| artifact.dkg_cycle.saturating_add(1))
        .unwrap_or(1);
    let recovered_epoch = recovered_boundary_artifact
        .map(|artifact| artifact.epoch)
        .unwrap_or(0);
    let vrf_safety = VrfSafetyGate::new(
        vrf_material_version,
        last_dkg_activation_height,
        dkg_rotation_params.planned_activation_height(last_dkg_activation_height),
        dkg_rotation_params.activation_grace_blocks,
    );
    info!(
        vrf_material_version,
        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
        last_dkg_activation_height,
        next_planned_activation_height = dkg_rotation_params
            .planned_activation_height(last_dkg_activation_height),
        vrf_expiry_height = dkg_rotation_params
            .planned_activation_height(last_dkg_activation_height)
            .saturating_add(dkg_rotation_params.activation_grace_blocks),
        "VRF material active"
    );
    publish_randomness_status(&bridge, &vrf_safety);

    // NOTE: is_fresh_bootstrap is determined AFTER marshal init (below),
    // using both execution height and consensus processed height.
    // This prevents false fresh-bootstrap on crash restart (SIGKILL/OOM)
    // where Reth lost in-memory state but consensus is durable.
    let dkg_manager = DkgManagerMailbox::new();
    let ancestry_readiness_target = last_consensus_finalized.get();
    let ancestry_readiness =
        AncestryReadiness::new(last_execution_height, ancestry_readiness_target);
    if !ancestry_readiness.is_ready() {
        info!(
            last_execution_height,
            last_consensus_finalized = ancestry_readiness_target,
            "marshal ancestry gate closed until executor backfills durable consensus blocks"
        );
    }
    // -- 9. Get beacon engine handle and payload builder handle ----------
    let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
    let payload_builder = node.payload_builder_handle.clone();

    // sus-5: the executor publishes execution-finalized heights here; the
    // supervisor consumes them to drive height-based DKG/VRF rotation. The
    // consumer arm is gated off while a reshare is in progress
    // (`if !reshare_in_progress`), so heights accumulate during a reshare. The
    // backlog is BOUNDED by the reshare duration (one height per finalized block
    // for the length of a reshare) and is drained in order afterwards. We keep an
    // ordered (unbounded) mpsc rather than a latest-only `watch` deliberately: the
    // drain feeds per-height rotation-threshold logic (freeze/activation heights),
    // so heights are processed in sequence rather than coalesced to the latest.
    let (executor_finalized_height_tx, mut executor_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (execution_finalized_height_tx, mut execution_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (consensus_tip_tx, mut consensus_tip_rx) =
        tokio::sync::watch::channel::<Option<crate::marshal_update_reporter::ConsensusTip>>(None);

    // Reth may have one or more speculative canonical blocks above marshal's
    // durable certified tip when the process stops. Those blocks remain useful
    // as local payload data, but they are not a finalization authority. Seed the
    // executor and FinalizationView at the highest height confirmed by both
    // stores so a different block winning at the first unfinalized height can
    // be imported and selected by forkchoice after restart.
    let recovery_anchor_height =
        durable_recovery_anchor_height(last_execution_height, last_consensus_finalized.get());
    let recovery_anchor_hash = if recovery_anchor_height == 0 {
        genesis_hash
    } else if recovery_anchor_height == last_execution_height {
        last_execution_hash
    } else {
        node.provider
            .block_hash(recovery_anchor_height)
            .map_err(|error| {
                eyre::eyre!(
                    "failed to read recovery-anchor block hash at height \
                     {recovery_anchor_height}: {error}"
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "missing canonical block hash for recovery anchor at height \
                     {recovery_anchor_height}"
                )
            })?
    };

    // -- 10. Create executor actor (state-aware init) --------------------
    let (mut executor_actor, executor_mailbox) = ExecutorActor::new(
        ctx.child("executor"),
        engine_handle.clone(),
        genesis_hash,
        recovery_anchor_height,
        recovery_anchor_hash,
        projection_readiness.clone(),
        Some(executor_finalized_height_tx),
    );

    // -- 12. Create application actor and handler ------------------------
    let (application, application_rx) =
        OutbeApplication::new(config::ENGINE_MAILBOX_SIZE, marshal_mailbox.clone());

    // -- 12d. Conditional bootstrap validation data ---------------------
    // Determined AFTER marshal init so we can use both execution height
    // and consensus processed height. Prevents false fresh-bootstrap on
    // crash restart where Reth lost in-memory state (SIGKILL/OOM) but
    // consensus layer persisted progress durably.
    // Reuse the same proven-founding decision that gated the canonical genesis
    // boundary and TEE OST3 ceremony. A verifier joining a running chain can have
    // both local heights at zero and must not seed genesis state.
    let is_fresh_bootstrap = coordinate_genesis_bootstrap;

    if last_execution_height == 0 && last_consensus_finalized.get() > 0 {
        info!(
            consensus_height = last_consensus_finalized.get(),
            "crash recovery detected - execution lost but consensus durable, will backfill"
        );
    }

    if is_fresh_bootstrap {
        use outbe_primitives::consensus::{GenesisValidator, GenesisValidators};

        let genesis_vals: Vec<GenesisValidator> = validator_set
            .addresses
            .iter()
            .zip(validator_set.public_keys.iter())
            .map(|(addr, pk)| {
                let pk_bytes = commonware_codec::Encode::encode(pk);
                let mut pubkey = [0u8; 48];
                let len = pk_bytes.len().min(48);
                pubkey[..len].copy_from_slice(&pk_bytes[..len]);
                GenesisValidator {
                    address: *addr,
                    consensus_pubkey: pubkey,
                }
            })
            .collect();

        bridge.set_genesis_validators(GenesisValidators {
            validators: genesis_vals,
            epoch_length_blocks,
        });

        let bootstrap_artifact = genesis_dkg_boundary_artifact.take().ok_or_else(|| {
            eyre::eyre!("fresh bootstrap lost its canonical genesis DKG boundary")
        })?;
        ensure!(
            bootstrap_artifact.vrf_material_version == vrf_material_version,
            "genesis DKG boundary VRF material version differs from the active startup version"
        );
        info!(
            vrf_material_version,
            vrf_group_public_key = %bootstrap_artifact.vrf_group_public_key,
            target_set_hash = %bootstrap_artifact.target_set_hash,
            active_set_hash = %bootstrap_artifact.reshare.active_set_hash,
            "genesis DKG boundary artifact queued; VRF active from view 2"
        );
        dkg_manager.note_bootstrap_outcome(bootstrap_artifact);
        info!("fresh bootstrap - genesis validators validation data queued");
    } else {
        info!(
            last_execution_height,
            last_consensus_finalized = last_consensus_finalized.get(),
            "ordinary restart - skipping genesis seeding"
        );
    }

    // Create broadcast engine for block dissemination.
    let (broadcast_engine, broadcast_mailbox) = commonware_broadcast::buffered::Engine::new(
        ctx.child("broadcast"),
        commonware_broadcast::buffered::Config {
            public_key: signing_key.public_key(),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            deque_size: config::BROADCAST_DEQUE_SIZE,
            peer_provider: oracle.clone(),
            priority: true,
            codec_config: (),
        },
    );

    // Initialize resolver for marshal.
    let resolver = marshal::resolver::p2p::init(
        ctx.child("marshal_resolver"),
        marshal::resolver::p2p::Config {
            public_key: signing_key.public_key(),
            peer_provider: oracle.clone(),
            blocker: oracle.clone(),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            timeout: std::time::Duration::from_secs(2),
            fetch_retry_timeout: std::time::Duration::from_millis(100),
            priority_requests: false,
            priority_responses: false,
        },
        marshal_channel,
    );

    // Start the marshal actor with a composite reporter.
    // Marshal delivers finalized blocks to executor via Reporter trait and
    // publishes finalized tips to provider-readiness/watchdog consumers.
    // Executor acknowledges after successful EL processing, which gates
    // marshal's processed height - the recovery truth on restart.
    let (peer_manager_actor, mut peer_manager_mailbox) = crate::peer_manager::Actor::new(
        ctx.child("peer_manager"),
        crate::peer_manager::Config {
            oracle: oracle.clone(),
            node: node.clone(),
            executor: executor_mailbox.clone(),
            bootnode_map: bootnode_map.clone(),
            initial_peers,
            initial_height: initial_peer_height,
        },
    );
    let mut peer_manager_handle_task = peer_manager_actor.start();

    let marshal_reporter =
        crate::marshal_update_reporter::MarshalUpdateReporter::new(executor_mailbox.clone())
            .add_tip_consumer(consensus_tip_tx.clone())
            .add_block_consumer(peer_manager_mailbox.clone());
    let mut marshal_handle =
        marshal_actor.start(marshal_reporter, broadcast_mailbox.clone(), resolver);

    // Serve `outbe_getFinalization` from the marshal so `--upstream` followers
    // can backfill + verify finalized blocks from this validator.
    spawn_finalization_drainer(&ctx, marshal_mailbox.clone(), bridge.clone());

    let (recovery_anchor_height, recovery_anchor_hash, recovered_finalized_round) =
        match recover_application_finalized_round(
            ctx.child("recover_application_finalized_round"),
            marshal_mailbox.clone(),
            last_execution_height,
        )
        .await
        {
            Ok(recovered) => reconcile_recovered_execution_head(
                last_execution_height,
                last_execution_hash,
                recovered,
            )?,
            Err(error) if args.trust_el_head => {
                warn!(
                    %error,
                    last_execution_height,
                    "marshal archive lacks finalized-round history; trusting the execution boundary"
                );
                (recovery_anchor_height, recovery_anchor_hash, None)
            }
            Err(head_error) => {
                // reth's canonical head can lead consensus finalization by the
                // in-flight block: one this node proposed and applied as its head
                // but had not finalized when it stopped (steady state:
                // head_height = finalized_height + 1). On a plain restart in that
                // window the head's finalization legitimately does not exist yet -
                // a normal unfinalized head, NOT archive corruption. Confirm the
                // marshal still holds its own finalized tip's record (a gap *there*
                // is genuine corruption) and that the head leads by a bounded
                // amount, then continue from marshal's durable finalized boundary.
                // The speculative Reth head remains available locally, but neither
                // ExecutorActor nor FinalizationView may call it finalized. The
                // network re-finalizes forward and Reth reorgs via forkchoice if a
                // different block wins the first unfinalized height.
                let finalized_tip = last_consensus_finalized.get();
                if !unfinalized_head_lead_is_recoverable(last_execution_height, finalized_tip) {
                    return Err(head_error);
                }
                let Ok(recovered_finalization) = recover_application_finalized_round(
                    ctx.child("recover_application_finalized_tip"),
                    marshal_mailbox.clone(),
                    finalized_tip,
                )
                .await
                else {
                    return Err(head_error);
                };
                let (certified_height, certified_hash, recovered_round) =
                    reconcile_recovered_execution_head(
                        finalized_tip,
                        recovery_anchor_hash,
                        recovered_finalization,
                    )
                    .wrap_err(
                        "marshal finalized-tip record disagrees with canonical execution history",
                    )?;
                warn!(
                    last_execution_height,
                    finalized_tip,
                    head_lead = last_execution_height.saturating_sub(finalized_tip),
                    recovery_anchor_hash = %recovery_anchor_hash,
                    "execution head leads the marshal finalized tip on restart; anchoring \
                     recovery at certified finality (unfinalized head re-finalized forward)"
                );
                (certified_height, certified_hash, recovered_round)
            }
        };

    let _recovered_ce_marker = recover_ce_at_reconciled_anchor(
        ce_startup_recovery.as_ref(),
        last_consensus_finalized.get(),
        recovery_anchor_height,
    )?;

    executor_actor = executor_actor.with_recovered_finalized_state(
        genesis_hash,
        recovery_anchor_height,
        recovery_anchor_hash,
    );

    // Restore certified provider finality before a queued payload can wait for
    // projection. The executor heartbeat cannot run while that wait is active.
    let recovery_checkpoint = ProjectionCheckpoint {
        block_number: recovery_anchor_height,
        block_hash: recovery_anchor_hash,
    };
    let fcu_provider_node = node.clone();
    confirm_recovered_forkchoice(
        ctx.child("recovered_forkchoice"),
        recovery_checkpoint,
        || executor_actor.replay_recovered_forkchoice_once(recovery_checkpoint),
        move || {
            read_reth_recovery_forkchoice(&fcu_provider_node.provider.canonical_in_memory_state())
        },
    )
    .await
    .wrap_err("failed to confirm recovered Reth forkchoice before validator startup")?;

    // Start broadcast engine with P2P channel.
    let _broadcast_handle = broadcast_engine.start(broadcast_channel);

    let application_epoch_fence = ApplicationEpochFence::new(Epoch::new(recovered_epoch));

    // -- Half B step 21: build the shared finalization view + block
    // cache BEFORE constructing the application handler. Both the
    // application handler (`build_block` reads `prev_randao` /
    // `last_timestamp_millis`; proposer inserts into `block_cache`) and
    // the FinalizationActor (sole writer for the view; evicts entries
    // below the new finalized height from `block_cache`) hold the same
    // `Arc`s. Recovery state is seeded into the view here.
    let finalization_view = new_finalization_view(
        recovery_anchor_hash,
        recovery_anchor_height,
        recovered_finalized_round,
    );
    let finalization_block_cache = BlockCache::new();

    // Construct the consensus-owned exact-parent certificate handoff store
    // before either the application handler (consumer-side waiter) or the
    // FinalizationActor (single writer) so both can clone from the same durable
    // backing.
    let parent_cert_dir = args
        .storage_dir
        .as_ref()
        .ok_or_else(|| eyre::eyre!("consensus storage_dir must be set before stack startup"))?
        .join("finalized_parent_certs");
    let finalized_parent_cert_store =
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore::open(
            &parent_cert_dir,
        )
        .wrap_err_with(|| {
            format!(
                "failed to open finalized parent certificate store at {}",
                parent_cert_dir.display()
            )
        })?;

    // Defensive startup hygiene: a crash between persisting a finalization parent
    // record and advancing the finalization view can leave an ahead-of-view
    // record on disk. Drop any finalization record above the recovered finalized
    // height so the store never retains a height the view has not reached.
    let pruned_ahead = finalized_parent_cert_store
        .prune_above_height(recovery_anchor_height)
        .wrap_err("failed to prune ahead-of-view finalized parent certificate records")?;
    if pruned_ahead > 0 {
        tracing::info!(
            pruned_ahead,
            recovered_finalized_height = recovery_anchor_height,
            "dropped ahead-of-recovered-view finalization parent records at startup"
        );
    }

    let ocomp_storage_root = args
        .storage_dir
        .as_ref()
        .expect("storage_dir was required above");
    let ocomp_retention_dir = ocomp_storage_root.join("ocomp_retention");
    let ocomp_proof_source = Arc::new(
        outbe_node::ocomp::retention::RethFinalizedInputProofSource::new(
            node.provider.clone(),
            finalized_parent_cert_store.clone(),
        ),
    );
    let ocomp_retention_coordinator = Arc::new(
        outbe_node::ocomp::retention::OcompRetentionCoordinator::open_with_retained_tributes_and_fence(
            ocomp_retention_dir,
            ocomp_proof_source,
            retained_tribute_writer,
            projection_retention_fence,
        ),
    );
    retention_selector
        .install(Arc::clone(&ocomp_retention_coordinator))
        .wrap_err("failed to install validator OCOMP retention selector")?;
    // Resolve consensus-sync block timings from genesis (timing.rs fallbacks,
    // no CLI override) once, before the handler ctor and the epoch loop.
    let bt = block_timing_from_genesis(&node)?;

    // one process-local late-finalize signature store shared by the
    // application handler (packs the proposer artifact), the FinalizationActor
    // (resolves views -> block numbers), and every per-epoch OutbeReporter
    // (records observed individual finalize votes). Best-effort, never consensus
    // state - the resulting artifact is re-verified pre-exec on every node.
    let late_sig_store = outbe_consensus::finalization::late_sig_store::shared(
        outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
    );

    // Create application handler with marshal mailbox and shared finalization state.
    let application_handler = ApplicationHandler::new(ApplicationDeps {
        unix_time_source: match args.testnet_unix_time_offset_secs {
            Some(offset_secs) => Arc::new(outbe_consensus::application::OffsetUnixTimeSource::new(
                offset_secs,
            )),
            None => Arc::new(outbe_consensus::application::SystemUnixTimeSource),
        },
        rx: application_rx,
        engine: engine_handle,
        payload_builder,
        executor_mailbox,
        genesis_hash,
        validators: validator_set.clone(),
        chain_id: node.chain_spec().chain().id(),
        ocomp_lifecycle_activation,
        marshal_mailbox: marshal_mailbox.clone(),
        certificate_scheme_provider: certificate_scheme_provider.clone(),
        elector_config_provider: elector_config_provider.clone(),
        committee_provider: committee_provider.clone(),
        dkg_manager: dkg_manager.clone(),
        vrf_safety: vrf_safety.clone(),
        epoch_fence: application_epoch_fence.clone(),
        ancestry_readiness: ancestry_readiness.clone(),
        projection_readiness,
        finalization_view: finalization_view.clone(),
        block_cache: finalization_block_cache.clone(),
        finalization_selector: outbe_consensus::finalization::selection::ParentProofSelector::new(
            finalized_parent_cert_store.clone(),
        ),
        payload_resolve_time: std::time::Duration::from_millis(args.payload_resolve_time_ms),
        min_block_time: bt.min_block_time,
        proposer_evm_address,
        trust_el_head: args.trust_el_head,
        late_sig_store: late_sig_store.clone(),
    });

    info!(
        last_execution_height,
        last_consensus_finalized = last_consensus_finalized.get(),
        "starting executor with recovery state"
    );

    // -- 13. Spawn persistent actors (survive engine restarts) -----------

    let execution_height_fanout = execution_finalized_height_tx.clone();
    let _ocomp_execution_ready_worker =
        ctx.child("ocomp_execution_ready")
            .spawn(move |_ctx| async move {
                while let Some(height) = executor_finalized_height_rx.recv().await {
                    let _ = execution_height_fanout.send(height);
                }
            });

    // Half B step 21: construct FinalizationActor + matching mailbox.
    // After step 21 the actor IS the production finalization path: the
    // OutbeReporter (constructed below per-epoch) sends finalizations
    // through `finalization_mailbox.notify_finalized` and the actor
    // owns all bridge / DKG / view-update side effects.
    //
    // Hand a clone of the exact-parent certificate store to the actor. The actor
    // is the only writer; the application handler reads hash-exact records via
    // the `ParentProofSelector` constructed above.
    let (finalization_actor, finalization_mailbox) =
        FinalizationActor::new(FinalizationActorDeps {
            view: finalization_view.clone(),
            block_cache: finalization_block_cache.clone(),
            marshal_mailbox: Some(marshal_mailbox.clone()),
            bridge: Some(bridge.clone()),
            dkg_manager: dkg_manager.clone(),
            vrf_safety: vrf_safety.clone(),
            parent_cert_store: finalized_parent_cert_store.clone(),
            certificate_scheme_provider: certificate_scheme_provider.clone(),
            late_sig_store: late_sig_store.clone(),
        });
    let mut finalization_handle = ctx
        .child("finalization")
        .spawn(move |ctx| finalization_actor.run(ctx));

    // persistent off-thread finalize-vote verifier. Each per-epoch
    // OutbeReporter enqueues raw finalize votes here instead of verifying
    // O(committee) BLS pairings inline on the Simplex voter task; the actor
    // resolves each vote's committee scheme by epoch through the shared
    // `certificate_scheme_provider` and admits only verified votes to
    // `late_sig_store`.
    let (finalize_verify_actor, finalize_verify_mailbox) =
        outbe_consensus::finalization::finalize_verify::FinalizeVerifyActor::new(
            certificate_scheme_provider.clone(),
            late_sig_store.clone(),
        );
    // Best-effort actor: its exit is non-fatal (consensus continues; only late
    // credits stop), so it is held for the engine's lifetime but not polled in
    // the fatal-exit select below. The named `_`-binding keeps the task alive
    // (a bare `_` would drop and abort it immediately).
    let _finalize_verify_handle = ctx
        .child("finalize_verify")
        .spawn(move |_ctx| finalize_verify_actor.run());

    let mut executor_handle_task = executor_actor
        .with_ancestry_readiness(ancestry_readiness.clone())
        .with_finalized_ce_committer(finalized_ce_committer)
        .start(marshal_mailbox.clone(), last_consensus_finalized);
    let mut handler_handle = ctx
        .child("application")
        .spawn(move |ctx| application_handler.run(ctx));

    info!("consensus actors and marshal block availability started");

    // ===================================================================
    // EPOCH LOOP - manages engine lifecycle and reshare triggering.
    // Each iteration creates a new Simplex engine with epoch-scoped
    // sub-channels. The engine is aborted when a reshare completes,
    // and a new engine starts at the next epoch.
    // ===================================================================
    let mut current_epoch = Epoch::new(recovered_epoch);
    let reporter_continuity = ReporterContinuity::default();

    // Channel for receiving DKG reshare results from background tasks.
    let (dkg_result_tx, mut dkg_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<DkgTaskOutcome>>();
    let (dkg_progress_tx, mut dkg_progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<dkg_actor::DkgProgress>();
    let mut reshare_in_progress = false;
    let mut frozen_dkg_target: Option<FrozenDkgTarget> = None;
    let mut pending_dkg_activation: Option<PendingDkgActivation> = None;
    let mut dealer_only_dkg_activation: Option<DealerOnlyDkgActivation> = None;
    let mut deferred_startup_pending_epoch: Option<Epoch> = None;
    if let Some(snapshot) = recovered_pending_boundary {
        let pending_epoch = Epoch::new(snapshot.artifact.epoch);
        let keys_dir = args.keys_dir.as_ref().ok_or_else(|| {
            eyre::eyre!("recovered pending DKG boundary without configured keys directory")
        })?;
        dkg_manager.note_recovered_pending_boundary(snapshot.artifact.clone());
        let restored = restore_pending_dkg_activation(
            snapshot,
            keys_dir,
            &key_backend,
            &signing_key.public_key(),
            &node,
        )?;
        let pending_artifact = match &restored {
            RestoredPendingDkgActivation::Participant(pending) => &pending.boundary_artifact,
            RestoredPendingDkgActivation::DealerOnly(pending) => {
                pending.boundary_artifact.as_ref().ok_or_else(|| {
                    eyre::eyre!("recovered dealer-only DKG handoff has no boundary artifact")
                })?
            }
        };
        let restored_target_dkg_cycle = match &restored {
            RestoredPendingDkgActivation::Participant(pending) => pending.target.dkg_cycle,
            RestoredPendingDkgActivation::DealerOnly(pending) => pending.target.dkg_cycle,
        };
        let exact_carrier_height = find_exact_finalized_preannounce_carrier(
            &node.provider,
            pending_artifact,
            recovery_anchor_height,
            dkg_rotation_params.activation_grace_blocks,
        )?;
        let startup_plan = startup_pending_dkg_epoch_plan(
            current_epoch,
            pending_epoch,
            recovery_anchor_height,
            pending_artifact.planned_activation_height,
            dkg_rotation_params.activation_grace_blocks,
            exact_carrier_height,
        )?;

        match startup_plan {
            StartupPendingDkgEpochPlan::Defer {
                active_epoch,
                preregister_after_current,
            } => {
                ensure!(
                    active_epoch == current_epoch,
                    "deferred startup DKG plan changed active epoch"
                );
                dkg_cycle =
                    next_dkg_cycle_after_restored_target(dkg_cycle, restored_target_dkg_cycle);
                match restored {
                    RestoredPendingDkgActivation::Participant(pending) => {
                        frozen_dkg_target = Some(pending.target.clone());
                        pending_dkg_activation = Some(pending);
                    }
                    RestoredPendingDkgActivation::DealerOnly(pending) => {
                        frozen_dkg_target = Some(pending.target.clone());
                        dealer_only_dkg_activation = Some(pending);
                    }
                }
                deferred_startup_pending_epoch = Some(preregister_after_current);
                info!(
                    active_epoch = %current_epoch,
                    pending_epoch = %preregister_after_current,
                    finalized_height = recovery_anchor_height,
                    "restored future DKG handoff; current-epoch channels will be acquired first"
                );
            }
            StartupPendingDkgEpochPlan::Activate {
                previous_epoch,
                active_epoch,
                activation_anchor,
            } => {
                let (target, canonical_output, activated_signing_share, boundary_artifact) =
                    match restored {
                        RestoredPendingDkgActivation::Participant(pending) => (
                            pending.target,
                            pending.complete.output,
                            Some(pending.complete.share),
                            pending.boundary_artifact,
                        ),
                        RestoredPendingDkgActivation::DealerOnly(pending) => {
                            let boundary_artifact = pending.boundary_artifact.ok_or_else(|| {
                                eyre::eyre!(
                                    "recovered dealer-only DKG activation has no boundary artifact"
                                )
                            })?;
                            let output = decode_boundary_output(&boundary_artifact).wrap_err(
                                "failed to decode recovered dealer-only DKG activation output",
                            )?;
                            (pending.target, output, None, boundary_artifact)
                        }
                    };
                ensure!(
                    boundary_artifact.epoch == active_epoch.get(),
                    "recovered DKG boundary epoch {} does not match activated startup epoch {}",
                    boundary_artifact.epoch,
                    active_epoch
                );
                let activated_vrf_material_version =
                    outbe_validatorset::next_vrf_material_version(vrf_material_version)?;
                ensure!(
                    boundary_artifact.vrf_material_version == activated_vrf_material_version,
                    "recovered DKG VRF material version {} does not follow active version {}",
                    boundary_artifact.vrf_material_version,
                    vrf_material_version
                );
                let activated_validator_set =
                    validator_set_for_dkg_output_players(&canonical_output, &target.validator_set)?;
                let activated_participants =
                    participants_from_validator_set(&activated_validator_set)?;

                signing_share = activated_signing_share;
                polynomial = canonical_output.public().clone();
                last_dkg_output = Some(canonical_output.clone());
                vrf_material_version = activated_vrf_material_version;
                activate_vrf_material_and_publish_local_share(
                    &bridge,
                    &vrf_materials,
                    vrf_material_version,
                    polynomial.clone(),
                    signing_share.clone(),
                );
                register_epoch_validation_providers(
                    active_epoch,
                    &activated_participants,
                    &activated_validator_set,
                    None,
                    &vrf_materials,
                    &certificate_scheme_provider,
                    &committee_provider,
                )?;
                let recovered_peer_map = build_peer_map(&activated_validator_set, &bootnode_map);
                eyre::ensure!(
                    peer_manager_mailbox.overwrite(recovered_peer_map)
                        == commonware_actor::Feedback::Ok,
                    "peer_manager closed during recovery admission"
                );
                validator_set = activated_validator_set;
                participants = activated_participants;
                dkg_cycle = target.dkg_cycle.saturating_add(1);
                last_dkg_activation_height = activation_anchor;
                vrf_safety.note_activated(
                    vrf_material_version,
                    activation_anchor,
                    dkg_rotation_params.planned_activation_height(activation_anchor),
                    dkg_rotation_params.activation_grace_blocks,
                );
                publish_randomness_status(&bridge, &vrf_safety);
                application_epoch_fence.arm_activation_boundary(previous_epoch, activation_anchor);
                application_epoch_fence.advance_epoch(active_epoch);
                current_epoch = active_epoch;
                frozen_dkg_target = None;
                pending_dkg_activation = None;
                dealer_only_dkg_activation = None;
                next_epoch_subchannels = Some(
                    outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                        active_epoch,
                        &mut vote_mux,
                        &mut cert_mux,
                        &mut res_mux,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "pre-register activated startup subchannels for epoch {active_epoch}"
                        )
                    })?,
                );
                info!(
                    previous_epoch = %previous_epoch,
                    active_epoch = %active_epoch,
                    activation_anchor,
                    finalized_height = recovery_anchor_height,
                    vrf_material_version,
                    dkg_cycle = target.dkg_cycle,
                    dkg_output_hash = %dkg_manager::dkg_output_hash(&canonical_output),
                    "restored preannounce-authorized DKG activation before boundary commit"
                );
            }
        }
    }
    let mut latest_consensus_tip = *consensus_tip_rx.borrow();
    let mut pending_provider_ready_height: Option<u64> = None;
    let mut watchdog_unhealthy_since: Option<SystemTime> = None;
    let watchdog_started_at = ctx.current();
    let mut provider_ready_retry_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(std::future::pending());
    let mut execution_watchdog_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
    let mut retry_frozen_dkg = false;
    info!(
        epoch_length_blocks = dkg_rotation_params.epoch_length_blocks,
        prepare_window_blocks = dkg_rotation_params.prepare_window_blocks,
        activation_grace_blocks = dkg_rotation_params.activation_grace_blocks,
        "configured block-based DKG/VRF rotation"
    );
    info!(
        min_block_time = ?bt.min_block_time,
        leader_timeout = ?bt.leader_timeout,
        certification_timeout = ?bt.certification_timeout,
        "consensus timeouts (genesis-sourced, no CLI override)"
    );
    'epoch_loop: loop {
        // -- a. Register or take pre-registered epoch sub-channels -------
        // Activation pre-registers `next_epoch_subchannels` at DKG
        // completion (see DKG completion handler below); the top of the
        // next iteration consumes it. The fallback path covers the
        // genesis-bootstrap iteration where no prior DKG completion has
        // run.
        let current_subchannels = if replacement_epoch_subchannels.is_some() {
            outbe_consensus::epoch_subchannels::take_or_register_current(
                current_epoch,
                &mut replacement_epoch_subchannels,
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await?
        } else {
            outbe_consensus::epoch_subchannels::take_or_register_current(
                current_epoch,
                &mut next_epoch_subchannels,
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await?
        };
        let outbe_consensus::epoch_subchannels::EpochSubchannels {
            vote, cert, res, ..
        } = current_subchannels;

        if let Some(pending_epoch) = deferred_startup_pending_epoch.take() {
            ensure!(
                next_epoch_subchannels.is_none(),
                "recovered future DKG handoff collided with an existing subchannel stash"
            );
            next_epoch_subchannels = Some(
                outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                    pending_epoch,
                    &mut vote_mux,
                    &mut cert_mux,
                    &mut res_mux,
                )
                .await
                .wrap_err_with(|| {
                    format!(
                        "pre-register recovered future-epoch subchannels after acquiring current epoch {current_epoch}"
                    )
                })?,
            );
            info!(
                active_epoch = %current_epoch,
                pending_epoch = %pending_epoch,
                "pre-registered recovered future DKG channels after current epoch"
            );
        }

        // -- b. Build HybridScheme for this epoch ------------------------
        use commonware_consensus::simplex::elector::Config as ElectorConfig;
        let radicle_signer = radicle_signer_enabled(
            radicle_status.snapshot().voting_gate,
            signing_share.is_some(),
        )?;
        let scheme = if radicle_signer {
            HybridScheme::<MinSig>::signer_with_vrf_provider(
                &config::outbe_app_namespace(),
                participants.clone(),
                signing_key.clone(),
                vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "signing key or BLS share invalid for validator set (epoch {current_epoch})"
                )
            })?
        } else {
            // Verifier mode (no threshold share this epoch): the engine follows and
            // verifies finalized blocks - driving its execution layer to sync - but
            // cannot propose or sign. `me()` is None, so the simplex engine never
            // invokes signing. The node acquires a share at the next reshare, after
            // which the next epoch iteration rebuilds this scheme as a signer (Stage 4).
            info!(
            target: "outbe_engine::stack",
                           epoch = %current_epoch,
                           "no threshold share for this epoch - running consensus engine in VERIFIER mode"
                       );
            HybridScheme::<MinSig>::verifier_with_vrf_provider(
                &config::outbe_app_namespace(),
                participants.clone(),
                vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "verifier scheme invalid for validator set (epoch {current_epoch}): \
                     polynomial total ({}) must equal participant count ({})",
                    vrf_materials.active_polynomial_total().unwrap_or(0),
                    participants.len(),
                )
            })?
        };

        // -- c. Create reporter for this epoch ---------------------------
        let recovered_boundary_for_epoch =
            recovered_boundary_artifact.filter(|artifact| artifact.epoch == current_epoch.get());
        let (verifier_scheme, ordered_addresses) = epoch_validation_inputs(
            current_epoch,
            &participants,
            &validator_set,
            recovered_boundary_for_epoch,
            &vrf_materials,
        )?;

        let elector_config =
            epoch_elector_config(current_epoch, &reporter_continuity, vrf_materials.clone())?;
        let reporter_elector = elector_config.clone().build(&participants);

        let _ = certificate_scheme_provider.register(current_epoch, verifier_scheme.clone());
        let _ = elector_config_provider.register(current_epoch, elector_config.clone());
        let _ = committee_provider.register(current_epoch, ordered_addresses.clone());

        let outbe_reporter = OutbeReporter::new(
            reporter_continuity.clone(),
            ordered_addresses,
            finalization_mailbox.clone(),
            Some(bridge.clone()),
            verifier_scheme,
            reporter_elector,
            current_epoch,
            std::sync::Arc::new(finalized_parent_cert_store.clone()),
            finalize_verify_mailbox.clone(),
        );

        // Combine OutbeReporter + marshal mailbox as a joint Simplex reporter.
        // Both receive Activity events including Finalization:
        // - OutbeReporter: bridge/VRF/missed-proposer processing AND
        // `Activity::Certification` -> CertifiedParentProofStore.
        // - Marshal: finalized block delivery -> executor -> ack -> recovery truth.
        //   Marshal's mailbox drops Certification via its `_ => return;` arm
        //   (monorepo `consensus/src/marshal/core/mailbox.rs:396-410`), so
        //   ordering between Outbe and marshal does not need to be sequential;
        //   `Reporters::from((outbe, marshal))` runs both via `futures::join!`
        //   and Outbe is the sole persistent consumer of Certification.
        let combined_reporter = Reporters::from((outbe_reporter, marshal_mailbox.clone()));

        // -- d. Resolve the Simplex genesis floor -------------------------
        // commonware 2026.5.0 removed `Automaton::genesis(epoch)`; the
        // genesis anchor is now an explicit `simplex::Config.floor`. We must
        // feed the byte-identical value the old `handle_genesis(epoch)`
        // returned:
        //   * epoch 0  -> the chain genesis block hash (`Digest(genesis_hash)`),
        //     the parent of `view = 1` for the bootstrap engine.
        //   * epoch > 0 -> the canonical last-finalized block's hash (the
        //     continuity anchor read from `FinalizationView`), the parent of
        //     `view = 1` for the restarted engine.
        // We use `Floor::Genesis(digest)` in both cases (never
        // `Floor::Finalized`) so behaviour matches the prior synthetic
        // `parent_view = 0` resolution path.
        //
        // The bounded-wait guard below preserves the prior epoch-restart
        // invariant: for `epoch > 0` we must not start the engine until the
        // FinalizationActor has published the boundary block's anchor, or the
        // floor (and Phase 1 finalized-round proof key) would be missing. The
        // 5s deadline accommodates transient races between the
        // FinalizationActor and the DKG-manager-driven epoch advance.
        let floor_digest = if current_epoch.get() == 0 {
            Digest(genesis_hash)
        } else {
            let deadline = ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
            loop {
                let (height, hash, round_ready) = {
                    let view = finalization_view.read();
                    (
                        view.last_finalized_number,
                        view.forkchoice.finalized_block_hash,
                        view.last_finalized_round.is_some(),
                    )
                };
                if height > 0 && hash != alloy_primitives::B256::ZERO && round_ready {
                    break Digest(hash);
                }
                if ctx.current() >= deadline {
                    return Err(eyre::eyre!(
                        "epoch={} restart without finalized anchor after {:?}; \
                         handle_genesis would return ZERO, or Phase 1 would lack \
                         the finalized-round proof key for parent_view=0",
                        current_epoch.get(),
                        EPOCH_RESTART_ANCHOR_TIMEOUT,
                    ));
                }
                ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
            }
        };

        // -- e. Build engine config --------------------------------------
        let simplex_cfg = simplex::Config {
            scheme,
            elector: elector_config,
            blocker: oracle.clone(),
            automaton: application.clone(),
            relay: application.clone(),
            forward: simplex::ForwardPolicy::Disabled,
            reporter: combined_reporter,
            strategy: commonware_parallel::Sequential,
            partition: format!("outbe-simplex-{}", current_epoch),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            epoch: current_epoch,
            floor: simplex::Floor::Genesis(floor_digest),
            replay_buffer: nonzero_usize(config::REPLAY_BUFFER, "REPLAY_BUFFER")?,
            write_buffer: nonzero_usize(config::WRITE_BUFFER, "WRITE_BUFFER")?,
            page_cache: page_cache.clone(),
            leader_timeout: bt.leader_timeout,
            certification_timeout: bt.certification_timeout,
            timeout_retry: config::DEFAULT_NULLIFY_REBROADCAST,
            view_retention: ViewDelta::new(u64::from(config::ACTIVITY_TIMEOUT)),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: config::DEFAULT_PEER_RESPONSE_TIMEOUT,
        };

        // -- f. Start engine ---------------------------------------------
        let engine = simplex::Engine::new(
            ctx.child("engine").with_attribute("epoch", current_epoch),
            simplex_cfg,
        );
        let mut engine_handle_task = engine.start(vote, cert, res);

        info!(epoch = %current_epoch, "simplex engine started - blocks can now be produced");

        // -- g. Engine event loop ----------------------------------------
        // Monitors engine, component exits, and block-height-driven reshare triggers.

        let epoch_loop_result: Result<EpochLoopOutcome> = async {
            let mut stack_shutdown = ctx.stopped();

            loop {
            let reshare_active = reshare_in_progress;
            let wait_for_execution_finalized_height = async {
                if reshare_active {
                    std::future::pending::<Option<u64>>().await
                } else {
                    execution_finalized_height_rx.recv().await
                }
            };
            commonware_macros::select! {
                _ = &mut stack_shutdown => {
                    info!(epoch = %current_epoch, "global stop received; draining simplex engine");
                    return Ok(EpochLoopOutcome::GlobalStop);
                },

                _ = &mut network_handle => {
                    info!("P2P network exited");
                    return Ok(EpochLoopOutcome::StackExit);
                },

                desired_signer = wait_for_radicle_role_change(
                    &mut radicle_updates,
                    radicle_signer,
                    signing_share.is_some(),
                ) => {
                    let desired_signer = desired_signer?;
                    info!(
                        epoch = %current_epoch,
                        previous_signer = radicle_signer,
                        desired_signer,
                        "canonical Radicle voting gate changed; replacing same-epoch Simplex role"
                    );
                    return Ok(EpochLoopOutcome::ReplaceSigner);
                },

                // Engine exit -> clean shutdown.
                result = &mut engine_handle_task => {
                    info!(epoch = %current_epoch, "simplex engine exited");
                    return Ok(EpochLoopOutcome::EngineExit(result));
                },

                // DKG reshare completed (from background task).
                Some(dkg_result) = dkg_result_rx.recv() => {
                    reshare_in_progress = false;
                    match dkg_result {
                        Ok(DkgTaskOutcome::Complete(dkg_complete)) => {
                            let Some(target) = frozen_dkg_target.take() else {
                                warn!("DKG completed without a frozen target; ignoring stale outcome");
                                outbe_consensus::metrics::record_dkg_status(0);
                                continue;
                            };

                            let current_height =
                                node.provider.last_block_number().map_err(|error| {
                                    eyre::eyre!(
                                        "failed to read latest block height after DKG completion: {error}"
                                    )
                                })?;
                            if frozen_dkg_target_expired(
                                current_height,
                                target.planned_activation_height,
                                dkg_rotation_params.activation_grace_blocks,
                            ) {
                                let deadline = target
                                    .planned_activation_height
                                    .saturating_add(
                                        dkg_rotation_params.activation_grace_blocks,
                                    );
                                vrf_safety.mark_expired(current_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "DKG completed without time for an outgoing-finalized preannounce: cycle {}, height {}, deadline {}",
                                    target.dkg_cycle,
                                    current_height,
                                    deadline
                                ));
                            }

                            let boundary_artifact = if let Some(ref keys_dir) = args.keys_dir {
                                persist_completed_dkg_before_activation(
                                    keys_dir,
                                    &key_backend,
                                    current_epoch,
                                    vrf_material_version,
                                    &participants,
                                    &target,
                                    &dkg_complete,
                                    current_height,
                                )?
                            } else {
                                build_completed_dkg_boundary(
                                    current_epoch,
                                    vrf_material_version,
                                    &participants,
                                    &target,
                                    &dkg_complete.output,
                                    &dkg_complete.participants,
                                )?
                            };

                            // Publish only after the exact artifact and threshold
                            // material are durable. Proposers can now carry this
                            // next-epoch artifact as a CommitteePreAnnounce before
                            // activation; the same immutable object is retained for
                            // the activation boundary below.
                            dkg_manager.note_ceremony_completed(boundary_artifact.clone());

                            info!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                is_validator_set_change = target.is_validator_set_change,
                                planned_activation_height = target.planned_activation_height,
                                current_height,
                                "DKG completed; waiting for exact outgoing-finalized preannounce carrier"
                            );
                            outbe_consensus::metrics::record_dkg_status(2); // completed
                            outbe_consensus::metrics::record_reshare_completed();
                            if current_height > target.planned_activation_height {
                                vrf_safety.note_grace(
                                    target.planned_activation_height,
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                            } else {
                                vrf_safety.note_pending_activation(
                                    target.planned_activation_height,
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                            }
                            publish_randomness_status(&bridge, &vrf_safety);

                            // Pre-register vote/cert/res sub-channels for
                            // the upcoming epoch BEFORE stashing the pending
                            // activation and BEFORE the
                            // `execution_finalized_height_tx.send(...)`
                            // call that may immediately wake the activation
                            // branch (when `should_activate_now` is true).
                            // This closes the cross-node race where a
                            // faster peer can begin broadcasting epoch-N+1
                            // traffic before this node has registered the
                            // matching sub-channel on its Mux. See
                            // `epoch_subchannels::register_epoch_subchannels`.
                            //
                            // Fail-fast: a Mux-level error
                            // (AlreadyRegistered, closed Mux) on a
                            // consensus-critical channel is a hard fault.
                            // No silent fallback to lazy registration.
                            let next_epoch =
                                next_consensus_epoch_after_dkg_activation(current_epoch);
                            if next_epoch_subchannels.is_some() {
                                warn!(
                                    epoch = %next_epoch,
                                    "stale DKG completion arrived after next-epoch subchannels were already pre-registered; ignoring"
                                );
                                continue;
                            }
                            next_epoch_subchannels = Some(
                                outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                                    next_epoch,
                                    &mut vote_mux,
                                    &mut cert_mux,
                                    &mut res_mux,
                                )
                                .await
                                .wrap_err_with(|| {
                                    format!(
                                        "pre-register next-epoch subchannels at DKG \
                                         completion for epoch {next_epoch}"
                                    )
                                })?,
                            );

                            pending_dkg_activation = Some(PendingDkgActivation {
                                target,
                                complete: dkg_complete,
                                boundary_artifact,
                                recovered_output: None,
                            });
                            let _ = execution_finalized_height_tx.send(current_height);
                        }
                        Ok(DkgTaskOutcome::DealerOnly(dealer_only_complete)) => {
                            let Some(target) = frozen_dkg_target.as_ref().cloned() else {
                                warn!("dealer-only DKG completed without a frozen target; ignoring stale outcome");
                                outbe_consensus::metrics::record_dkg_status(0);
                                continue;
                            };
                            if dealer_only_complete.participants != target.participants {
                                return Err(eyre::eyre!(
                                    "dealer-only DKG participant set does not match frozen target"
                                ));
                            }

                            let current_height =
                                node.provider.last_block_number().map_err(|error| {
                                    eyre::eyre!(
                                        "failed to read latest block height after dealer-only DKG completion: {error}"
                                    )
                                })?;
                            if frozen_dkg_target_expired(
                                current_height,
                                target.planned_activation_height,
                                dkg_rotation_params.activation_grace_blocks,
                            ) {
                                let deadline = target
                                    .planned_activation_height
                                    .saturating_add(
                                        dkg_rotation_params.activation_grace_blocks,
                                    );
                                vrf_safety.mark_expired(current_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "dealer-only DKG completed without time for an outgoing-finalized preannounce: cycle {}, height {}, deadline {}",
                                    target.dkg_cycle,
                                    current_height,
                                    deadline
                                ));
                            }
                            info!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                planned_activation_height = target.planned_activation_height,
                                current_height,
                                "dealer-only DKG completed; remaining in old validator set until activation"
                            );
                            dealer_only_dkg_activation = Some(DealerOnlyDkgActivation {
                                target,
                                boundary_artifact: None,
                                recovered_output: None,
                            });
                            outbe_consensus::metrics::record_dkg_status(2);
                            outbe_consensus::metrics::record_reshare_completed();
                            let _ = execution_finalized_height_tx.send(current_height);
                        }
                        Err(e) => {
                            // Height notifications are deliberately not consumed while a
                            // ceremony is running. Check the authoritative finalized view
                            // before scheduling a retry: otherwise an old queued height can
                            // start another ceremony while the chain is already at the VRF
                            // deadline, and the application cannot propose the next block
                            // that would wake this branch again.
                            if let Some(target) = frozen_dkg_target.as_ref() {
                                let current_height =
                                    finalization_view.read().last_finalized_number;
                                if frozen_dkg_target_expired(
                                    current_height,
                                    target.planned_activation_height,
                                    dkg_rotation_params.activation_grace_blocks,
                                ) {
                                    let activation_deadline = target
                                        .planned_activation_height
                                        .saturating_add(
                                            dkg_rotation_params.activation_grace_blocks,
                                        );
                                    vrf_safety.mark_expired(current_height);
                                    publish_randomness_status(&bridge, &vrf_safety);
                                    return Err(eyre::eyre!(
                                        "frozen DKG target missed VRF expiry: cycle {}, height {}, deadline {}",
                                        target.dkg_cycle,
                                        current_height,
                                        activation_deadline
                                    ));
                                }
                            }
                            warn!(?e, "DKG reshare failed, retrying frozen target on next check");
                            retry_frozen_dkg = true;
                        }
                    }
                },

                Some(progress) = dkg_progress_rx.recv() => {
                    match progress {
                        dkg_actor::DkgProgress::LocalDealerLog(bytes) => {
                            if let Err(error) = dkg_manager.note_local_dealer_log(current_epoch, bytes) {
                                warn!(%error, epoch = %current_epoch, "failed recording local dealer log");
                            }
                        }
                        dkg_actor::DkgProgress::P2pDealerLog(bytes) => {
                            if let Err(error) = dkg_manager.note_pending_dealer_log(current_epoch, bytes) {
                                warn!(%error, epoch = %current_epoch, "failed recording P2P dealer log candidate");
                            }
                        }
                    }
                },

                _ = &mut execution_watchdog_timer => {
                    execution_watchdog_timer =
                        Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
                    let Some(tip) = latest_consensus_tip else {
                        debug!("execution watchdog waiting for first consensus tip");
                        continue;
                    };

                    let consensus_tip_height = tip.height.get();
                    let mut reth_head_height = match node.provider.last_block_number() {
                        Ok(height) => height,
                        Err(error) => {
                            let now = ctx.current();
                            let (decision, next_unhealthy_since) = execution_watchdog_decision(
                                ExecutionWatchdogObservation::ProviderReadError,
                                now,
                                watchdog_started_at,
                                watchdog_unhealthy_since,
                            );
                            watchdog_unhealthy_since = next_unhealthy_since;
                            match decision {
                                ExecutionWatchdogDecision::StartupGrace => {
                                    let startup_elapsed = elapsed_since(now, watchdog_started_at);
                                    warn!(
                                        %error,
                                        startup_elapsed_ms = startup_elapsed.as_millis(),
                                        startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                        "execution watchdog failed to read Reth provider head during startup/backfill grace"
                                    );
                                }
                                ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                                    warn!(
                                        %error,
                                        unhealthy_for_ms = unhealthy_for.as_millis(),
                                        "execution watchdog failed to read Reth provider head"
                                    );
                                }
                                ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                                    return Err(eyre::eyre!(
                                        "execution watchdog provider read failed for {:?}: {error}",
                                        unhealthy_for
                                    ));
                                }
                                ExecutionWatchdogDecision::Healthy => {}
                            }
                            continue;
                        }
                    };
                    let provider_tip_hash = match node.provider.block_hash(consensus_tip_height) {
                        Ok(hash) => hash,
                        Err(error) => {
                            let now = ctx.current();
                            let (decision, next_unhealthy_since) = execution_watchdog_decision(
                                ExecutionWatchdogObservation::ProviderReadError,
                                now,
                                watchdog_started_at,
                                watchdog_unhealthy_since,
                            );
                            watchdog_unhealthy_since = next_unhealthy_since;
                            match decision {
                                ExecutionWatchdogDecision::StartupGrace => {
                                    let startup_elapsed = elapsed_since(now, watchdog_started_at);
                                    warn!(
                                        %error,
                                        consensus_tip_height,
                                        consensus_tip_digest = %tip.digest,
                                        reth_head_height,
                                        startup_elapsed_ms = startup_elapsed.as_millis(),
                                        startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                        "execution watchdog failed to read Reth provider hash at consensus tip during startup/backfill grace"
                                    );
                                }
                                ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                                    warn!(
                                        %error,
                                        consensus_tip_height,
                                        consensus_tip_digest = %tip.digest,
                                        reth_head_height,
                                        unhealthy_for_ms = unhealthy_for.as_millis(),
                                        "execution watchdog failed to read Reth provider hash at consensus tip"
                                    );
                                }
                                ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                                    return Err(eyre::eyre!(
                                        "execution watchdog provider hash read failed at consensus tip height {} for {:?}: {error}",
                                        consensus_tip_height,
                                        unhealthy_for
                                    ));
                                }
                                ExecutionWatchdogDecision::Healthy => {}
                            }
                            continue;
                        }
                    };
                    let hash_match = provider_tip_hash == Some(tip.digest.0);
                    match node.provider.last_block_number() {
                        Ok(height) => {
                            reth_head_height = height;
                        }
                        Err(error) => {
                            warn!(
                                %error,
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                previous_reth_head_height = reth_head_height,
                                "execution watchdog failed to refresh Reth provider head after consensus tip hash probe; using previous height sample"
                            );
                        }
                    }
                    outbe_consensus::metrics::record_consensus_reth_state(
                        consensus_tip_height,
                        reth_head_height,
                        hash_match,
                    );

                    let consensus_ahead = consensus_tip_height.saturating_sub(reth_head_height);
                    let now = ctx.current();
                    let (decision, next_unhealthy_since) = execution_watchdog_decision(
                        ExecutionWatchdogObservation::ProviderState {
                            consensus_tip_height,
                            reth_head_height,
                            hash_match,
                        },
                        now,
                        watchdog_started_at,
                        watchdog_unhealthy_since,
                    );
                    watchdog_unhealthy_since = next_unhealthy_since;
                    match decision {
                        ExecutionWatchdogDecision::Healthy => {}
                        ExecutionWatchdogDecision::StartupGrace => {
                            let startup_elapsed = elapsed_since(now, watchdog_started_at);
                            warn!(
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                reth_head_height,
                                ?provider_tip_hash,
                                consensus_ahead,
                                hash_match,
                                startup_elapsed_ms = startup_elapsed.as_millis(),
                                startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                "execution watchdog detected Reth provider behind consensus tip during startup/backfill grace"
                            );
                        }
                        ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                            warn!(
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                reth_head_height,
                                ?provider_tip_hash,
                                consensus_ahead,
                                hash_match,
                                unhealthy_for_ms = unhealthy_for.as_millis(),
                                "execution watchdog detected Reth provider behind consensus tip"
                            );
                        }
                        ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                            return Err(eyre::eyre!(
                                "execution watchdog fatal: Reth provider head/hash not ready for consensus tip height {} digest {} (reth_head={}, provider_tip_hash={:?}, unhealthy_for={:?})",
                                consensus_tip_height,
                                tip.digest,
                                reth_head_height,
                                provider_tip_hash,
                                unhealthy_for,
                            ));
                        }
                    }
                },

                consensus_tip_changed = consensus_tip_rx.changed() => {
                    match consensus_tip_changed {
                        Ok(()) => {
                            latest_consensus_tip = *consensus_tip_rx.borrow_and_update();
                            if let Some(current_height) = pending_provider_ready_height {
                                let _ = execution_finalized_height_tx.send(current_height);
                            }
                        }
                        Err(error) => {
                            warn!(%error, "consensus tip watch channel closed");
                        }
                    }
                },

                _ = &mut provider_ready_retry_timer => {
                    provider_ready_retry_timer = Box::pin(std::future::pending());
                    if let Some(current_height) = pending_provider_ready_height {
                        let _ = execution_finalized_height_tx.send(current_height);
                    }
                },

                // Block-height based DKG/VRF rotation. This is driven by execution-finalized
                // height notifications after successful new_payload + FCU, not wall-clock
                // polling or raw consensus finalization.
                Some(current_height) = wait_for_execution_finalized_height => {
                    match latest_consensus_tip {
                        Some(tip) => {
                            if !provider_matches_consensus_tip(&node.provider, tip, current_height)? {
                                pending_provider_ready_height = Some(current_height);
                                provider_ready_retry_timer =
                                    Box::pin(ctx.sleep(config::DEFAULT_PEER_RESPONSE_TIMEOUT));
                                debug!(
                                    current_height,
                                    consensus_tip_height = tip.height.get(),
                                    consensus_tip_digest = %tip.digest,
                                    "provider not ready for DKG/VRF scheduling; retrying"
                                );
                                continue;
                            }
                            pending_provider_ready_height = None;
                            provider_ready_retry_timer = Box::pin(std::future::pending());
                            if let Some(boundary) =
                                dkg_manager.take_committed_boundary_artifact().await
                            {
                                let boundary_output = decode_boundary_output(&boundary)
                                    .wrap_err("failed to decode finalized DKG boundary output")?;
                                if last_dkg_output.as_ref() != Some(&boundary_output) {
                                    let local_pk = signing_key.public_key();
                                    if boundary_output.players().position(&local_pk).is_none() {
                                        if let Some(ref keys_dir) = args.keys_dir {
                                            retire_activated_dkg_retry_state(
                                                keys_dir,
                                                &key_backend,
                                            )?;
                                        }
                                        info!(
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "finalized DKG boundary excludes local validator; exiting validator mode"
                                        );
                                        return Ok(EpochLoopOutcome::StackExit);
                                    }
                                    return Err(eyre::eyre!(
                                        "finalized DKG boundary output does not match active local DKG output"
                                    ));
                                }
                                if let Some(ref keys_dir) = args.keys_dir {
                                    if let Some(share) = signing_share.as_ref() {
                                        save_dkg_state(
                                            keys_dir,
                                            share,
                                            &polynomial,
                                            &boundary_output,
                                            &key_backend,
                                        )
                                        .wrap_err(
                                            "failed to promote finalized DKG state to disk",
                                        )?;
                                        info!(
                                            keys_dir = %keys_dir.display(),
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "promoted finalized DKG state to durable storage"
                                        );
                                    } else {
                                        info!(
                                            keys_dir = %keys_dir.display(),
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "finalized DKG boundary adopted in verifier mode; no private share to promote"
                                        );
                                    }
                                    retire_activated_dkg_retry_state(keys_dir, &key_backend)?;
                                }
                            }
                        }
                        None => {
                            pending_provider_ready_height = Some(current_height);
                            debug!(
                                current_height,
                                "no consensus tip available for DKG/VRF scheduling; retrying"
                            );
                            continue;
                        }
                    }

                    if let Some(pending) = pending_dkg_activation.as_ref() {
                        let consensus_finalized_height = finalization_view
                            .read()
                            .last_finalized_number
                            .min(current_height);
                        let exact_carrier_height = find_exact_finalized_preannounce_carrier(
                            &node.provider,
                            &pending.boundary_artifact,
                            consensus_finalized_height,
                            dkg_rotation_params.activation_grace_blocks,
                        )?;
                        match pending_dkg_handoff_decision(
                            consensus_finalized_height,
                            pending.target.planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                            exact_carrier_height,
                        ) {
                            PendingDkgHandoffDecision::Expired { deadline } => {
                                vrf_safety.mark_expired(consensus_finalized_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "pending DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    pending.target.dkg_cycle,
                                    consensus_finalized_height,
                                    deadline
                                ));
                            }
                            PendingDkgHandoffDecision::Wait => {}
                            PendingDkgHandoffDecision::Activate { activation_anchor: activation_height } => {
                                let Some(canonical_output) = select_pending_canonical_output(
                                    dkg_manager.canonical_output(current_epoch),
                                    pending.recovered_output.as_ref(),
                                )
                                else {
                                    warn!(
                                        epoch = %current_epoch,
                                        activation_height,
                                        "DKG activation height reached but canonical finalized-log output is not ready"
                                    );
                                    continue;
                                };
                                let Some(pending) = pending_dkg_activation.take() else {
                                    return Err(eyre::eyre!(
                                        "pending DKG activation missing after precheck at height {activation_height}"
                                    ));
                                };
                            let target = pending.target;
                            let dkg_complete = pending.complete;
                            let boundary_artifact = pending.boundary_artifact;
                            if let Err(error) = dkg_manager::assert_canonical_output(
                                &dkg_complete.output,
                                &canonical_output,
                                &format!("cycle {}", target.dkg_cycle),
                            ) {
                                vrf_safety.mark_expired(activation_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(error);
                            }

                            let activated_validator_set = validator_set_for_dkg_output_players(
                                &canonical_output,
                                &target.validator_set,
                            )?;
                            let activated_participants =
                                participants_from_validator_set(&activated_validator_set)?;
                            let activated_is_validator_set_change = activated_participants != participants;
                            // invariant:
                            // `vrf_material_version` increments by exactly 1 per
                            // successful reshare activation. Overflow is a
                            // deterministic activation error, not saturation.
                            // The single source of truth lives in the
                            // `outbe-validatorset` crate so proposer and
                            // validator paths cannot diverge.
                            let activated_vrf_material_version =
                                outbe_validatorset::next_vrf_material_version(
                                    vrf_material_version,
                                )?;
                            let activated_polynomial = canonical_output.public().clone();
                            let activated_signing_share = Some(dkg_complete.share);
                            let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);
                            ensure!(
                                boundary_artifact.epoch == next_epoch.get(),
                                "completed DKG boundary epoch {} does not match activation epoch {}",
                                boundary_artifact.epoch,
                                next_epoch.get()
                            );
                            let published_boundary = dkg_manager
                                .pending_boundary_artifact(next_epoch)
                                .await
                                .ok_or_else(|| {
                                    eyre::eyre!(
                                        "completed DKG boundary is missing from manager at activation"
                                    )
                                })?;
                            ensure!(
                                published_boundary == boundary_artifact,
                                "published pre-announce boundary diverged before activation"
                            );
                            let epoch_boundary_height =
                                activation_height.max(target.planned_activation_height);

                            vrf_material_version = activated_vrf_material_version;
                            polynomial = activated_polynomial;
                            last_dkg_output = Some(canonical_output.clone());
                            signing_share = activated_signing_share;
                            activate_vrf_material_and_publish_local_share(
                                &bridge,
                                &vrf_materials,
                                vrf_material_version,
                                polynomial.clone(),
                                signing_share.clone(),
                            );

                            application_epoch_fence.arm_activation_boundary(
                                current_epoch,
                                epoch_boundary_height,
                            );
                            debug!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                max_block_height = epoch_boundary_height,
                                "armed application epoch fence for DKG activation"
                            );

                            last_dkg_activation_height =
                                activation_height.max(target.planned_activation_height);
                            vrf_safety.note_activated(
                                vrf_material_version,
                                last_dkg_activation_height,
                                dkg_rotation_params
                                    .planned_activation_height(last_dkg_activation_height),
                                dkg_rotation_params.activation_grace_blocks,
                            );
                            info!(
 target: "outbe_engine::stack",
                                dkg_cycle = target.dkg_cycle,
                                activation_height = last_dkg_activation_height,
                                planned_activation_height = target.planned_activation_height,
                                vrf_material_version,
                                vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                                dkg_output_hash = %dkg_manager::dkg_output_hash(&canonical_output),
                                dkg_public_polynomial_hash = %dkg_manager::public_polynomial_hash(&polynomial),
                                is_validator_set_change = activated_is_validator_set_change,
                                "VRF/DKG material activated"
                            );
                            publish_randomness_status(&bridge, &vrf_safety);
                            register_epoch_validation_providers(
                                next_epoch,
                                &activated_participants,
                                &activated_validator_set,
                                None,
                                &vrf_materials,
                                &certificate_scheme_provider,
                                &committee_provider,
                            )?;
                            validator_set = activated_validator_set;
                            frozen_dkg_target = None;
                            outbe_consensus::metrics::record_dkg_status(0);

                            participants = activated_participants;
                            application_epoch_fence.advance_epoch(next_epoch);
                            engine_handle_task.abort();
                            current_epoch = next_epoch;
                            info!(
                                epoch = %current_epoch,
                                vrf_material_version,
                                is_validator_set_change = activated_is_validator_set_change,
                                "DKG activation advanced consensus epoch; restarting Simplex engine"
                            );
                            // DKG activation race: before bouncing back
                            // into the epoch loop (which will call `engine.start`
                            // for the new epoch), wait for the FinalizationActor
                            // to publish the activation block as finalized. The
                            // generic `current_epoch > 0` guard at the top of
                            // the loop checks only that *some* finalized anchor
                            // exists, which is a weaker condition than
                            // `last_finalized_number >= activation_height` - a
                            // stale anchor would still satisfy the generic
                            // guard while pointing Simplex at the wrong parent.
                            let activation_height = last_dkg_activation_height;
                            let deadline =
                                ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
                            loop {
                                let (finalized, finalized_hash, round_ready) = {
                                    let view = finalization_view.read();
                                    (
                                        view.last_finalized_number,
                                        view.forkchoice.finalized_block_hash,
                                        view.last_finalized_round.is_some(),
                                    )
                                };
                                if finalized >= activation_height
                                    && finalized_hash != alloy_primitives::B256::ZERO
                                    && round_ready
                                {
                                    break;
                                }
                                if ctx.current() >= deadline {
                                    return Err(eyre::eyre!(
                                        "DKG activation race after {:?}: \
                                         finalized_anchor=(height={}, hash={}, round_ready={}) \
                                         activation_height={}; \
                                         FinalizationActor is lagging the DKG manager",
                                        EPOCH_RESTART_ANCHOR_TIMEOUT,
                                        finalized,
                                        finalized_hash,
                                        round_ready,
                                        activation_height
                                    ));
                                }
                                ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
                            }
                            return Ok(EpochLoopOutcome::RestartEpoch);
                            }
                        }
                    }

                    if dealer_only_dkg_activation
                        .as_ref()
                        .is_some_and(|pending| pending.boundary_artifact.is_none())
                    {
                        if let Some(canonical_output) = dkg_manager.canonical_output(current_epoch) {
                            let target = dealer_only_dkg_activation
                                .as_ref()
                                .map(|pending| pending.target.clone())
                                .ok_or_else(|| eyre::eyre!("dealer-only activation disappeared while preparing boundary"))?;
                            let boundary_artifact = if let Some(ref keys_dir) = args.keys_dir {
                                persist_observed_dkg_boundary_before_activation(
                                    keys_dir,
                                    current_epoch,
                                    vrf_material_version,
                                    &participants,
                                    &target,
                                    &canonical_output,
                                    current_height,
                                )?
                            } else {
                                build_completed_dkg_boundary(
                                    current_epoch,
                                    vrf_material_version,
                                    &participants,
                                    &target,
                                    &canonical_output,
                                    &target.participants,
                                )?
                            };
                            dkg_manager.note_ceremony_completed(boundary_artifact.clone());
                            if next_epoch_subchannels.is_none() {
                                let next_epoch =
                                    next_consensus_epoch_after_dkg_activation(current_epoch);
                                next_epoch_subchannels = Some(
                                    outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                                        next_epoch,
                                        &mut vote_mux,
                                        &mut cert_mux,
                                        &mut res_mux,
                                    )
                                    .await
                                    .wrap_err_with(|| {
                                        format!(
                                            "pre-register next-epoch subchannels for dealer-only handoff epoch {next_epoch}"
                                        )
                                    })?,
                                );
                            }
                            let pending = dealer_only_dkg_activation
                                .as_mut()
                                .ok_or_else(|| eyre::eyre!("dealer-only activation disappeared before boundary publication"))?;
                            pending.boundary_artifact = Some(boundary_artifact);
                            info!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                "dealer-only DKG reconstructed and published durable pending boundary"
                            );
                        }
                    }

                    let dealer_only_decision = dealer_only_dkg_activation.as_ref().and_then(|d| {
                        d.boundary_artifact.as_ref().map(|boundary_artifact| {
                            (
                                d.target.planned_activation_height,
                                d.target.dkg_cycle,
                                boundary_artifact.clone(),
                            )
                        })
                    });
                    if let Some((planned_activation_height, target_dkg_cycle, boundary_artifact)) =
                        dealer_only_decision
                    {
                        let consensus_finalized_height = finalization_view
                            .read()
                            .last_finalized_number
                            .min(current_height);
                        let exact_carrier_height = find_exact_finalized_preannounce_carrier(
                            &node.provider,
                            &boundary_artifact,
                            consensus_finalized_height,
                            dkg_rotation_params.activation_grace_blocks,
                        )?;
                        match pending_dkg_handoff_decision(
                            consensus_finalized_height,
                            planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                            exact_carrier_height,
                        ) {
                            PendingDkgHandoffDecision::Expired { deadline } => {
                                vrf_safety.mark_expired(consensus_finalized_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "dealer-only DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    target_dkg_cycle,
                                    consensus_finalized_height,
                                    deadline
                                ));
                            }
                            PendingDkgHandoffDecision::Wait => {}
                            PendingDkgHandoffDecision::Activate { activation_anchor: activation_height } => {
                                // S3 demotion: an exited validator (deactivated/unstaked) is a
                                // previous-output dealer but not a frozen-target player, so it
                                // finishes its dealer duties for the resharded committee and then,
                                // instead of looping until VRF expiry kills the process, DEMOTES to
                                // a share-less verifier-follower of the smaller (N-1) committee. It
                                // adopts the new group polynomial reconstructed from the finalized
                                // dealer logs it just helped produce (`canonical_output` for the
                                // ceremony epoch), drops its share, advances its epoch, and restarts
                                // the Simplex engine in verifier mode - the same finalized-follower
                                // path a non-staked TEE full-node uses. The reshared output is a
                                // membership change, so unlike the same-membership verifier-follow
                                // the node MUST take the new polynomial + participant set here (it
                                // has them from the ceremony) rather than reusing the old ones.
                                let canonical_output = select_pending_canonical_output(
                                    dkg_manager.canonical_output(current_epoch),
                                    dealer_only_dkg_activation
                                        .as_ref()
                                        .and_then(|pending| pending.recovered_output.as_ref()),
                                )
                                    .ok_or_else(|| eyre::eyre!(
                                        "dealer-only pending boundary lost its canonical output before activation"
                                    ))?;
                                let next_epoch =
                                    next_consensus_epoch_after_dkg_activation(current_epoch);
                                let published_boundary = dkg_manager
                                    .pending_boundary_artifact(next_epoch)
                                    .await
                                    .ok_or_else(|| {
                                        eyre::eyre!(
                                            "dealer-only completed DKG boundary is missing from manager at activation"
                                        )
                                    })?;
                                ensure!(
                                    published_boundary == boundary_artifact,
                                    "dealer-only published preannounce boundary diverged before activation"
                                );
                                let Some(dealer_only) = dealer_only_dkg_activation.take() else {
                                    return Err(eyre::eyre!(
                                        "dealer-only activation missing after decision at height {activation_height}"
                                    ));
                                };
                                let target = dealer_only.target;
                                let activated_validator_set =
                                    validator_set_for_dkg_output_players(
                                        &canonical_output,
                                        &target.validator_set,
                                    )?;
                                let activated_participants =
                                    participants_from_validator_set(&activated_validator_set)?;
                                info!(
                                    epoch = %current_epoch,
                                    next_epoch = %next_epoch,
                                    activation_height,
                                    old = participants.len(),
                                    new = activated_participants.len(),
                                    "shareless verifier: authenticated DKG handoff complete"
                                );
                                let new_vrf_material_version =
                                    match outbe_validatorset::next_vrf_material_version(
                                        vrf_material_version,
                                    ) {
                                        Ok(version) => version,
                                        Err(error) => {
                                            warn!(%error, "exited validator: vrf material version overflow during demotion; reusing current");
                                            vrf_material_version
                                        }
                                    };
                                signing_share = None;
                                polynomial = canonical_output.public().clone();
                                last_dkg_output = Some(canonical_output);
                                validator_set = activated_validator_set;
                                participants = activated_participants;
                                vrf_material_version = new_vrf_material_version;
                                dkg_cycle = target.dkg_cycle.saturating_add(1);
                                activate_vrf_material_and_publish_local_share(
                                    &bridge,
                                    &vrf_materials,
                                    vrf_material_version,
                                    polynomial.clone(),
                                    None,
                                );
                                register_epoch_validation_providers(
                                    next_epoch,
                                    &participants,
                                    &validator_set,
                                    None,
                                    &vrf_materials,
                                    &certificate_scheme_provider,
                                    &committee_provider,
                                )?;
                                let anchored_height = activation_height;
                                last_dkg_activation_height = activation_height;
                                vrf_safety.note_activated(
                                    vrf_material_version,
                                    anchored_height,
                                    dkg_rotation_params.planned_activation_height(anchored_height),
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                                publish_randomness_status(&bridge, &vrf_safety);
                                application_epoch_fence.arm_activation_boundary(
                                    current_epoch,
                                    activation_height,
                                );
                                frozen_dkg_target = None;
                                application_epoch_fence.advance_epoch(next_epoch);
                                engine_handle_task.abort();
                                current_epoch = next_epoch;
                                // Anchor wait (mirror the verifier activation): the restarted
                                // verifier engine's floor needs the activation block finalized
                                // before `'epoch_loop` rebuilds the verifier scheme.
                                let deadline =
                                    ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
                                loop {
                                    let (finalized, finalized_hash, round_ready) = {
                                        let view = finalization_view.read();
                                        (
                                            view.last_finalized_number,
                                            view.forkchoice.finalized_block_hash,
                                            view.last_finalized_round.is_some(),
                                        )
                                    };
                                    if finalized >= activation_height
                                        && finalized_hash != alloy_primitives::B256::ZERO
                                        && round_ready
                                    {
                                        break;
                                    }
                                    if ctx.current() >= deadline {
                                        return Err(eyre::eyre!(
                                            "exited-validator demotion activation race after {:?}: \
                                             finalized=(height={}, hash={}, round_ready={}) activation_height={}",
                                            EPOCH_RESTART_ANCHOR_TIMEOUT,
                                            finalized,
                                            finalized_hash,
                                            round_ready,
                                            activation_height
                                        ));
                                    }
                                    ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
                                }
                                return Ok(EpochLoopOutcome::RestartEpoch);
                            }
                        }
                    }

                    if let Some(target) = frozen_dkg_target.as_ref() {
                        let activation_deadline = target
                            .planned_activation_height
                            .saturating_add(dkg_rotation_params.activation_grace_blocks);
                        if frozen_dkg_target_expired(
                            current_height,
                            target.planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                        ) {
                            vrf_safety.mark_expired(current_height);
                            publish_randomness_status(&bridge, &vrf_safety);
                            return Err(eyre::eyre!(
                                "frozen DKG target missed VRF expiry: cycle {}, height {}, deadline {}",
                                target.dkg_cycle,
                                current_height,
                                activation_deadline
                            ));
                        }
                    }

                    if retry_frozen_dkg {
                        retry_frozen_dkg = false;
                        if let Some(target) = frozen_dkg_target.as_ref().cloned() {
                            info!(
                                dkg_cycle = target.dkg_cycle,
                                planned_activation_height = target.planned_activation_height,
                                "retrying DKG for frozen target"
                            );
                            reshare_in_progress = true;
                            outbe_consensus::metrics::record_dkg_status(1);

                            match dkg_mux.register(target.dkg_cycle).await {
                                Ok((dkg_tx, dkg_rx)) => {
                                    let round = target.dkg_cycle;
                                    let tx = dkg_result_tx.clone();
                                    let progress_tx = dkg_progress_tx.clone();
                                    let key = signing_key.clone();
                                    let parts = target.participants.clone();
                                    // Share-less joiner: refresh prev_output from the chain so
                                    // the ceremony info_hash matches the committee's (see
                                    // refresh_verifier_join_prev_output).
                                    if signing_share.is_none() {
                                        refresh_verifier_join_prev_output(
                                            &node.provider,
                                            target.freeze_height,
                                            dkg_rotation_params,
                                            &mut last_dkg_output,
                                        );
                                    }
                                    let prev_output = last_dkg_output.clone();
                                    let prev_share = signing_share.clone();
                                    let role = classify_local_reshare_role(
                                        &key.public_key(),
                                        prev_output.as_ref(),
                                        &parts,
                                    );
                                    let (finalized_log_tx, finalized_log_rx) =
                                        tokio::sync::mpsc::unbounded_channel();
                                    if let Err(error) = restart_dkg_manager_from_finalized_history(
                                        &node.provider,
                                        &dkg_manager,
                                        DkgCeremonyReplaySpec {
                                            epoch: current_epoch,
                                            round,
                                            previous_output: prev_output.clone(),
                                            participants: target.participants.clone(),
                                            finalized_dealer_log_tx: Some(finalized_log_tx.clone()),
                                        },
                                        target.freeze_height,
                                        current_height,
                                        || {
                                            (*consensus_tip_rx.borrow()).expect(
                                                "the height arm continues before DKG retry when no consensus tip is available",
                                            )
                                        },
                                    ) {
                                        warn!(%error, epoch = %current_epoch, round, "failed to recover DKG manager state for frozen-target retry");
                                        reshare_in_progress = false;
                                        retry_frozen_dkg = true;
                                        outbe_consensus::metrics::record_dkg_status(0);
                                        continue;
                                    }
                                    let retry_store = dkg_retry_store(&args, &key_backend)?;
                                    ctx.child("dkg_retry").spawn(move |dkg_ctx| async move {
                                        let result = match role {
                                            LocalDkgRole::DealerAndPlayer => {
                                                dkg_actor::run_initial_dkg_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    prev_share,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::PlayerOnly => {
                                                dkg_actor::run_initial_dkg_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    None,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                                (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    output,
                                                    share,
                                                    round,
                                                    progress_tx,
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::DealerOnly),
                                                (None, _) => Err(eyre::eyre!(
                                                    "dealer-only DKG retry requires previous output"
                                                )),
                                                (Some(_), None) => Err(eyre::eyre!(
                                                    "dealer-only DKG requires a previous share"
                                                )),
                                            },
                                            LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                                "local key is neither previous dealer nor target player for DKG retry"
                                            )),
                                        };
                                        let _ = tx.send(result);
                                    });
                                }
                                Err(e) => {
                                    warn!(?e, "failed to register DKG subchannel for retry");
                                    reshare_in_progress = false;
                                    retry_frozen_dkg = true;
                                }
                            }
                            continue;
                        }
                    }

                    let freeze_height = dkg_rotation_params.freeze_height(last_dkg_activation_height);
                    if dealer_only_dkg_activation.is_none()
                        && should_start_dkg_rotation(
                            frozen_dkg_target.is_some(),
                            pending_dkg_activation.is_some(),
                            current_height,
                            freeze_height,
                        )
                    {
                        let planned_activation_height =
                            dkg_rotation_params.planned_activation_height(last_dkg_activation_height);
                        info!(
                            dkg_cycle,
                            current_height,
                            freeze_height,
                            planned_activation_height,
                            "freezing validator set and starting DKG rotation"
                        );

                        // Freeze the target set from the EVM state at freeze_height.
                        // This keeps DKG membership deterministic across validators.
                        let (target_validator_set, target_participants, tee_expired_target_exclusions) = match refresh_validator_set_at_height(&node, freeze_height) {
                            Ok(FrozenValidatorSetRefresh::Ready {
                                validator_set: new_set,
                                participants: new_participants,
                                tee_expired_target_exclusions,
                            }) => {
                                let old_count = participants.len();
                                let local_role = classify_local_reshare_role(
                                    &signing_key.public_key(),
                                    last_dkg_output.as_ref(),
                                    &new_participants,
                                );
                                if local_role == LocalDkgRole::NotParticipant {
                                    // A share-less verifier-follower does not run a ceremony,
                                    // but it observes the same finalized dealer logs, reconstructs
                                    // the exact incoming output, publishes/validates the same
                                    // preannounce, and crosses the same outgoing-finalized
                                    // handoff as participants. Reusing the old polynomial at the
                                    // planned height would bypass authentication and cannot follow
                                    // membership-changing rotations safely.
                                    if signing_share.is_none() {
                                        let peer_map = build_peer_map(&new_set, &bootnode_map);
                                        peer_manager_mailbox.prepare_dkg(peer_map).await
                                            .wrap_err("failed to publish verifier-follower DKG admission")?;
                                        restart_dkg_manager_from_finalized_history(
                                            &node.provider,
                                            &dkg_manager,
                                            DkgCeremonyReplaySpec {
                                                epoch: current_epoch,
                                                round: dkg_cycle,
                                                previous_output: last_dkg_output.clone(),
                                                participants: new_participants.clone(),
                                                finalized_dealer_log_tx: None,
                                            },
                                            freeze_height,
                                            current_height,
                                            || {
                                                (*consensus_tip_rx.borrow()).expect(
                                                    "the height arm continues before verifier-follower DKG recovery when no consensus tip is available",
                                                )
                                            },
                                        )
                                        .wrap_err(
                                            "failed to recover verifier-follower DKG reconstruction from finalized history",
                                        )?;
                                        let is_validator_set_change =
                                            new_participants != participants;
                                        let target = FrozenDkgTarget {
                                            dkg_cycle,
                                            freeze_height,
                                            planned_activation_height,
                                            validator_set: new_set,
                                            participants: new_participants,
                                            tee_expired_target_exclusions,
                                            is_validator_set_change,
                                        };
                                        info!(
                                            freeze_height,
                                            planned_activation_height,
                                            dkg_cycle,
                                            "verifier-follower: reconstructing pending DKG boundary before authenticated handoff"
                                        );
                                        frozen_dkg_target = Some(target.clone());
                                        dealer_only_dkg_activation =
                                            Some(DealerOnlyDkgActivation {
                                                target,
                                                boundary_artifact: None,
                                                recovered_output: None,
                                            });
                                        outbe_consensus::metrics::record_dkg_status(2);
                                        let _ = execution_finalized_height_tx.send(current_height);
                                        continue;
                                    }
                                    return Err(eyre::eyre!(
                                        "local validator is neither previous DKG dealer nor frozen target player at height {freeze_height}"
                                    ));
                                }

                                // Update P2P oracle so new validators can participate in DKG.
                                let peer_map = build_peer_map(&new_set, &bootnode_map);
                                let dkg_peer_set_id = peer_manager_mailbox.prepare_dkg(peer_map).await
                                    .wrap_err("failed to publish DKG admission")?;

                                info!(
                                    old = old_count,
                                    new = new_participants.len(),
                                    ?local_role,
                                    dkg_peer_set_id,
                                    tee_expired_target_exclusions = tee_expired_target_exclusions.len(),
                                    "refreshed validator set from EVM state for reshare"
                                );
                                (new_set, new_participants, tee_expired_target_exclusions)
                            }
                            Ok(FrozenValidatorSetRefresh::PendingBlockHash) => {
                                match pending_freeze_block_hash_decision(
                                    current_height,
                                    planned_activation_height,
                                ) {
                                    PendingFreezeBlockHashDecision::Retry => {}
                                    PendingFreezeBlockHashDecision::Expired => {
                                        vrf_safety.mark_expired(current_height);
                                        publish_randomness_status(&bridge, &vrf_safety);
                                        return Err(eyre::eyre!(
                                            "frozen validator set block hash unavailable by planned activation: freeze height {freeze_height}, current height {current_height}, planned activation {planned_activation_height}"
                                        ));
                                    }
                                }
                                warn!(
                                    current_height,
                                    freeze_height,
                                    planned_activation_height,
                                    "frozen validator set block hash is not available yet; retrying on next finalized height"
                                );
                                continue;
                            }
                            Err(e) => {
                                vrf_safety.mark_expired(current_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "failed to refresh frozen validator set at height {freeze_height}: {e}"
                                ));
                            }
                        };

                        let is_validator_set_change = target_participants != participants;
                        let target_dkg_cycle = dkg_cycle;
                        frozen_dkg_target = Some(FrozenDkgTarget {
                            dkg_cycle: target_dkg_cycle,
                            freeze_height,
                            planned_activation_height,
                            validator_set: target_validator_set.clone(),
                            participants: target_participants.clone(),
                            tee_expired_target_exclusions,
                            is_validator_set_change,
                        });
                        vrf_safety.note_preparing(
                            target_dkg_cycle,
                            freeze_height,
                            planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                        );
                        publish_randomness_status(&bridge, &vrf_safety);
                        dkg_cycle = target_dkg_cycle.saturating_add(1);

                        reshare_in_progress = true;
                        outbe_consensus::metrics::record_dkg_status(1); // in progress

                        // Register DKG sub-channel for this reshare round.
                        match dkg_mux.register(target_dkg_cycle).await {
                            Ok((dkg_tx, dkg_rx)) => {
                                let round = target_dkg_cycle;
                                let tx = dkg_result_tx.clone();
                                let progress_tx = dkg_progress_tx.clone();
                                let key = signing_key.clone();
                                let parts = target_participants.clone();
                                // Share-less joiner (verifier-join becoming a player): refresh
                                // prev_output from the chain's canonical output so the ceremony
                                // info_hash matches the committee's. Without this a long-lived
                                // TEE full-node joining at its FIRST reshare presents its stale
                                // CLI `--consensus.dkg-output` -> info_hash mismatch -> dealer
                                // bundles dropped -> timeout -> ACTIVE-but-voteless.
                                if signing_share.is_none() {
                                    refresh_verifier_join_prev_output(
                                        &node.provider,
                                        freeze_height,
                                        dkg_rotation_params,
                                        &mut last_dkg_output,
                                    );
                                }
                                // Capture previous DKG state for reshare.
                                let prev_output = last_dkg_output.clone();
                                let prev_share = signing_share.clone();
                                let role = classify_local_reshare_role(
                                    &key.public_key(),
                                    prev_output.as_ref(),
                                    &parts,
                                );
                                let (finalized_log_tx, finalized_log_rx) =
                                    tokio::sync::mpsc::unbounded_channel();
                                if let Err(error) = restart_dkg_manager_from_finalized_history(
                                    &node.provider,
                                    &dkg_manager,
                                    DkgCeremonyReplaySpec {
                                        epoch: current_epoch,
                                        round,
                                        previous_output: prev_output.clone(),
                                        participants: target_participants.clone(),
                                        finalized_dealer_log_tx: Some(finalized_log_tx.clone()),
                                    },
                                    freeze_height,
                                    current_height,
                                    || {
                                        (*consensus_tip_rx.borrow()).expect(
                                            "the height arm continues before live DKG recovery when no consensus tip is available",
                                        )
                                    },
                                ) {
                                    warn!(
                                        %error,
                                        epoch = %current_epoch,
                                        round,
                                        from_height = freeze_height,
                                        scheduling_height = current_height,
                                        "failed to recover live DKG manager state from finalized history"
                                    );
                                    reshare_in_progress = false;
                                    retry_frozen_dkg = true;
                                    outbe_consensus::metrics::record_dkg_status(0);
                                    continue;
                                }
                                let retry_store = dkg_retry_store(&args, &key_backend)?;
                                ctx.child("dkg_live").spawn(move |dkg_ctx| async move {
                                    let result = match role {
                                        LocalDkgRole::DealerAndPlayer => {
                                            dkg_actor::run_initial_dkg_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                prev_share,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::PlayerOnly => {
                                            dkg_actor::run_initial_dkg_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                None,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                            (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                output,
                                                share,
                                                round,
                                                progress_tx,
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::DealerOnly),
                                            (None, _) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires previous output"
                                            )),
                                            (Some(_), None) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires a previous share"
                                            )),
                                        },
                                        LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                            "local key is neither previous dealer nor target player for live DKG"
                                        )),
                                    };
                                    let _ = tx.send(result);
                                });
                            }
                            Err(e) => {
                                warn!(?e, "failed to register DKG subchannel");
                                reshare_in_progress = false;
                                frozen_dkg_target = None;
                            }
                        }
                    } else {
                        debug!(
                            current_height,
                            freeze_height,
                            "DKG rotation freeze height not reached"
                        );
                    }
                },

                // Component exits -> fatal.
                result = &mut executor_handle_task => {
                    info!("executor actor exited");
                    let executor_result = result
                        .map_err(|e| eyre::eyre!("executor actor task failed: {e:?}"))?;
                    executor_result.wrap_err("executor actor returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut handler_handle => {
                    info!("application handler exited");
                    let application_result = result
                        .map_err(|e| eyre::eyre!("application handler task failed: {e:?}"))?;
                    application_result.wrap_err("application handler returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut finalization_handle => {
                    info!("finalization actor exited");
                    let finalization_result = result
                        .map_err(|e| eyre::eyre!("finalization actor task failed: {e:?}"))?;
                    finalization_result.wrap_err("finalization actor returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut peer_manager_handle_task => {
                    info!("peer manager actor exited");
                    result.map_err(|e| eyre::eyre!("peer manager actor exited: {e:?}"))?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // SSA-8: the marshal actor is consensus-liveness-critical (block
                // availability, finalized-block delivery to the executor). With
                // `catch_panics`, a marshal panic (e.g. an unacknowledged Exact,
                // or a future telemetry-label assert) resolves its handle instead
                // of aborting the process - so an UNmonitored handle would leave
                // the node silently stalled (no blocks delivered, consensus
                // wedged). Monitor it like the other components: a marshal exit
                // is fatal and shuts the node down with the cause.
                result = &mut marshal_handle => {
                    info!("marshal actor exited");
                    result.map_err(|e| eyre::eyre!("marshal actor exited: {e:?}"))?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // The broadcast (buffered dissemination) handle remains managed by
                // the Commonware runtime; its failure degrades to the marshal
                // pull/serve path rather than a consensus stall.
            }
            }
        }
        .await;

        match supervise_epoch_loop_result(
            &ctx,
            epoch_loop_result,
            &mut engine_handle_task,
            &application_drain,
        )
        .await?
        {
            EpochLoopAction::RestartEpoch => continue 'epoch_loop,
            EpochLoopAction::ReplaceSigner => {
                replacement_epoch_subchannels = Some(
                    outbe_consensus::epoch_subchannels::reacquire_epoch_subchannels(
                        current_epoch,
                        &ctx,
                        Duration::from_secs(5),
                        Duration::from_millis(10),
                        &mut vote_mux,
                        &mut cert_mux,
                        &mut res_mux,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "reacquire same-epoch channels while replacing Radicle role in epoch {current_epoch}"
                        )
                    })?,
                );
                continue 'epoch_loop;
            }
            EpochLoopAction::ExitStack => break 'epoch_loop,
        }
    }

    // Bridge is kept alive for the duration of consensus - executor reads from it.
    drop(bridge);

    Ok(())
}

/// Build the leader-elector config for an epoch start.
///
/// Epoch 0 has no previous finalized certificate, so view 1 uses the one-time
/// genesis round-robin exception. Every later epoch must start from the last
/// finalized certificate of the previous epoch so that view 1 continues to use
/// VRF-derived leader selection rather than silently falling back to round-robin.
pub(in crate::stack) fn epoch_elector_config(
    epoch: Epoch,
    continuity: &ReporterContinuity,
    vrf_materials: VrfMaterialProvider<MinSig>,
) -> Result<HybridRandom<MinSig>> {
    if epoch.get() == 0 {
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }

    let snapshot = continuity.snapshot();
    if snapshot.last_finalized_view == 0 {
        warn!(
            epoch = epoch.get(),
            "starting epoch without reporter continuity; leader election will use active VRF material until a certificate is finalized"
        );
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }
    let seed = snapshot.last_vrf_seed.unwrap_or_default();
    if seed.is_empty() {
        Ok(HybridRandom::with_vrf_materials(vrf_materials))
    } else {
        Ok(HybridRandom::with_bootstrap_seed_and_vrf_materials(
            seed,
            vrf_materials,
        ))
    }
}
