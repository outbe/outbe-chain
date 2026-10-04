use super::super::*;

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
    let radicle_updates = radicle_status.subscribe();

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
            services::FollowerConnection {
                node,
                bridge,
                upstream,
            },
            services::FollowerStackServices {
                projection_readiness,
                ocomp_readiness,
                retained_tribute_writer,
                projection_retention_fence,
                retention_selector,
                finalized_ce_committer,
                ce_startup_recovery,
                follower_shutdown: follower_shutdown
                    .ok_or_else(|| eyre::eyre!("follower pre-stop handshake is not installed"))?,
            },
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
    let validator_set =
        validators::read_consensus_validators_at_block(&node.provider, initial_peer_hash)
            .wrap_err("failed to load consensus validator set at startup")?;

    info!(
        count = validator_set.public_keys.len(),
        "loaded validator set"
    );

    let Some(transport) = super::transport::start_transport(
        &ctx,
        &args,
        &node,
        super::transport::TransportAdmission {
            signing_key: &signing_key,
            validator_set: &validator_set,
            initial_peer_hash,
            ocomp_install_hash,
        },
        radicle_endpoint,
    )
    .await?
    else {
        return Ok(());
    };
    let super::transport::StartupTransport {
        network_handle,
        oracle,
        bootnode_map,
        initial_peers,
        broadcast_channel,
        marshal_channel,
        vote_mux,
        cert_mux,
        res_mux,
        mut dkg_mux,
        tee_dkg_round0,
        tee_bootstrap_round0,
        tee_dkg_mux: _tee_dkg_mux,
        tee_bootstrap_mux: _tee_bootstrap_mux,
    } = transport;
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

    let certificate_scheme_provider = HybridSchemeProvider::<MinSig>::new();
    let elector_config_provider = HybridElectorConfigProvider::<MinSig>::new();
    let committee_provider = CommitteeProvider::new();

    let super::marshal_recovery::RecoveredMarshal {
        actor: marshal_actor,
        mailbox: marshal_mailbox,
        processed_height: last_consensus_finalized,
    } = super::marshal_recovery::recover_marshal(
        &ctx,
        &page_cache,
        epoch_length_blocks,
        &node,
        &certificate_scheme_provider,
    )
    .await?;

    let super::threshold_recovery::RecoveredThreshold {
        local_consensus_key,
        last_execution_height,
        last_execution_hash,
        recovered_boundary,
        recovered_pending_boundary,
        signing_share,
        mut polynomial,
        mut last_dkg_output,
        coordinate_genesis_bootstrap,
        mut genesis_dkg_boundary_artifact,
        participants,
        proposer_evm_address,
    } = super::threshold_recovery::recover_threshold(
        &ctx,
        super::threshold_recovery::ThresholdRecovery {
            args: &args,
            node: &node,
            key_backend: &key_backend,
            signing_key: &signing_key,
            validator_set: &validator_set,
            genesis_hash,
            dkg_rotation_params,
            last_consensus_finalized,
            dkg_mux: &mut dkg_mux,
        },
    )
    .await?;
    let shareless_verifier = signing_share.is_none();
    super::tee_bootstrap::prepare_tee(
        &ctx,
        super::tee_bootstrap::TeeStartup {
            args: &args,
            node: &node,
            bridge: &bridge,
            participants: &participants,
            validator_set: &validator_set,
            local_consensus_key: &local_consensus_key,
            proposer_evm_address,
            coordinate_genesis_bootstrap,
            shareless_verifier,
            genesis_dkg_boundary_artifact: &genesis_dkg_boundary_artifact,
            tee_dkg_round0,
            tee_bootstrap_round0,
        },
    )
    .await?;

    // -- 8. Recover execution finalized state ------------------------------
    let active_boundary = recovered_boundary.clone();

    let recovered_boundary_artifact = active_boundary.as_ref().map(|(_, artifact)| artifact);
    let vrf_material_version = recovered_boundary_artifact
        .map(|artifact| artifact.vrf_material_version)
        .unwrap_or(0);
    reconcile_recovered_vrf_material(
        &mut polynomial,
        &mut last_dkg_output,
        signing_share.is_some(),
        recovered_boundary_artifact,
    )?;
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
    let last_dkg_activation_height = active_boundary
        .as_ref()
        .map(|(height, _)| *height)
        .unwrap_or(last_execution_height);
    let dkg_cycle = recovered_boundary_artifact
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
    let (execution_finalized_height_tx, execution_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (consensus_tip_tx, consensus_tip_rx) =
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
    let (application, application_rx) = OutbeApplication::new(
        ctx.child("application"),
        config::ENGINE_MAILBOX_SIZE,
        marshal_mailbox.clone(),
    );

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
    let (peer_manager_actor, peer_manager_mailbox) = crate::peer_manager::Actor::new(
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
    let peer_manager_handle_task = peer_manager_actor.start();

    let marshal_reporter =
        crate::marshal_update_reporter::MarshalUpdateReporter::new(executor_mailbox.clone())
            .with_publication(application.publication())
            .add_tip_consumer(consensus_tip_tx.clone())
            .add_block_consumer(peer_manager_mailbox.clone());
    let marshal_handle = marshal_actor.start(marshal_reporter, broadcast_mailbox.clone(), resolver);

    // Serve `outbe_getFinalization` from the marshal so `--upstream` followers
    // can backfill + verify finalized blocks from this validator.

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

    spawn_finalization_drainer(
        &ctx,
        marshal_mailbox.clone(),
        bridge.clone(),
        finalized_parent_cert_store.clone(),
    );

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
        publication: application.publication(),
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
    let finalization_handle = ctx
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

    let executor_handle_task = executor_actor
        .with_ancestry_readiness(ancestry_readiness.clone())
        .with_finalized_ce_committer(finalized_ce_committer)
        .start(marshal_mailbox.clone(), last_consensus_finalized);
    let handler_handle = ctx
        .child("application")
        .spawn(move |ctx| application_handler.run(ctx));

    info!("consensus actors and marshal block availability started");

    use super::runtime::{
        DkgRotation, EpochChannels, EpochState, EpochSupervisor, PersistentActors,
    };
    let (dkg_result_tx, dkg_result_rx) = tokio::sync::mpsc::unbounded_channel();
    let (dkg_progress_tx, dkg_progress_rx) = tokio::sync::mpsc::unbounded_channel();
    EpochSupervisor {
        state: EpochState {
            current_epoch: Epoch::new(recovered_epoch),
            validator_set,
            participants,
            signing_share,
            polynomial,
            last_dkg_output,
            vrf_material_version,
            last_dkg_activation_height,
            dkg_cycle,
        },
        rotation: DkgRotation {
            reshare_in_progress: false,
            frozen_dkg_target: None,
            pending_dkg_activation: None,
            dealer_only_dkg_activation: None,
            deferred_startup_pending_epoch: None,
            retry_frozen_dkg: false,
            dkg_mux,
            dkg_result_tx,
            dkg_result_rx,
            dkg_progress_tx,
            dkg_progress_rx,
        },
        channels: EpochChannels {
            vote_mux,
            cert_mux,
            res_mux,
            next_epoch_subchannels: None,
            replacement_epoch_subchannels: None,
        },
        actors: PersistentActors {
            network_handle,
            executor_handle_task,
            handler_handle,
            finalization_handle,
            peer_manager_handle_task,
            marshal_handle,
        },
        args,
        node,
        bridge,
        signing_key,
        key_backend,
        dkg_rotation_params,
        dkg_manager,
        vrf_materials,
        vrf_safety,
        application_epoch_fence,
        certificate_scheme_provider,
        elector_config_provider,
        committee_provider,
        peer_manager_mailbox,
        bootnode_map,
        oracle,
        recovered_boundary_artifact: recovered_boundary_artifact.cloned(),
        reporter_continuity: ReporterContinuity::default(),
        genesis_hash,
        bt,
        page_cache,
        application,
        marshal_mailbox,
        finalization_mailbox,
        finalization_view,
        finalized_parent_cert_store,
        finalize_verify_mailbox,
        application_drain,
        radicle_status,
        radicle_updates,
        execution_finalized_height_rx,
        execution_finalized_height_tx,
        consensus_tip_rx,
    }
    .run(ctx, recovered_pending_boundary, recovery_anchor_height)
    .await
}

fn reconcile_recovered_vrf_material(
    polynomial: &mut Sharing<MinSig>,
    last_dkg_output: &mut Option<Output<MinSig, bls12381::PublicKey>>,
    has_signing_share: bool,
    recovered_boundary_artifact: Option<&DkgBoundaryArtifact>,
) -> Result<()> {
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
    if has_signing_share {
        validate_recovered_vrf_material(polynomial, recovered_boundary_artifact)?;
        if let (Some(output), Some(boundary)) =
            (last_dkg_output.as_ref(), recovered_boundary_artifact)
        {
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
        *polynomial = canonical_output.public().clone();
        *last_dkg_output = Some(canonical_output);
    }
    Ok(())
}
