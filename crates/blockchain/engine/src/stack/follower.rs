use super::*;

/// Spawn the drainer that answers `outbe_getFinalization` RPC requests from the
/// marshal. The `outbe-rpc` handler cannot see the marshal or `ConsensusBlock`,
/// so it requests bytes through [`ConsensusExecutionBridge::request_finalization`];
/// this task is the consensus-side responder. Wired on BOTH the validator path
/// (`run_consensus_stack`) and the certified-follower path (a follower can serve
/// upstream too), right after `marshal_mailbox` exists.
///
/// For each `(height, reply)` it reads the finalization certificate and the
/// finalized block from the marshal, encodes both with `commonware_codec`, and
/// answers `Some` only when both are present locally (otherwise `None`, which
/// the RPC maps to a "not available" error).
pub(in crate::stack) fn spawn_finalization_drainer<E>(
    ctx: &E,
    marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
    bridge: ConsensusExecutionBridge,
) where
    E: Spawner + Metrics,
{
    let rx = bridge.set_finalization_fetcher();
    ctx.child("finalization_drainer")
        .spawn(move |_| async move {
            let mut rx = rx;
            while let Some((height, reply)) = rx.recv().await {
                let answer =
                    finalization_bytes_for_height(&marshal_mailbox, Height::new(height)).await;
                // The receiver may have gone away (RPC client disconnected); ignore.
                let _ = reply.send(answer);
            }
        });
}

/// Read `(finalization, block)` for `height` from the marshal and encode them
/// for transport. `None` if either is missing locally.
async fn finalization_bytes_for_height(
    marshal_mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
    height: Height,
) -> Option<outbe_primitives::consensus::FinalizedBlockBytes> {
    use commonware_codec::Encode as _;

    let finalization = marshal_mailbox.get_finalization(height).await?;
    // The block is keyed by the finalization's payload digest.
    let block = marshal_mailbox
        .get_block(&finalization.proposal.payload)
        .await?;
    Some(outbe_primitives::consensus::FinalizedBlockBytes {
        finalization: alloy_primitives::Bytes::from(finalization.encode().to_vec()),
        block: alloy_primitives::Bytes::from(block.encode().to_vec()),
    })
}

/// Run the consensus stack.
///
/// Wires together:
/// 1. Validator configuration (static JSON or dynamic from EVM state)
/// 2. HybridScheme signing (BLS individual + BLS12-381 threshold VRF)
/// 3. P2P network channels (lookup::Network) with Muxers for epoch-scoped sub-channels
/// 4. Application handler (propose/verify via beacon engine)
/// 5. Executor actor (FCU updates, finalization)
/// 6. Simplex consensus engine (restarted on reshare)
/// 7. Block propagation - proposer broadcasts full blocks via P2P channel
/// 8. Automatic reshare detection and DKG execution
///
/// Follower stack: cold-sync finalized blocks from an upstream node, verify them
/// against the trusted network identity (committee-chaining - see the `follow`
/// module), and drive the EL via the existing executor, WITHOUT running the
/// consensus engine. Selected by `--upstream`.
#[allow(clippy::too_many_arguments)]
pub(in crate::stack) async fn run_follow_stack<E>(
    ctx: E,
    args: ConsensusArgs,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    upstream: String,
    projection_readiness: ProjectionReadinessHandle,
    ocomp_readiness: Option<ProjectionReadinessHandle>,
    retained_tribute_writer: Arc<RetainedTributeWriter>,
    projection_retention_fence: Arc<ProjectionRetentionFence>,
    retention_selector: Arc<SharedOcompRetentionSelector>,
    finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    ce_startup_recovery: Arc<dyn CeStartupRecovery>,
    follower_shutdown: crate::follower_shutdown::FollowerDrain,
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
    let epoch_length = epoch_length_blocks_from_genesis(&node)?;

    if args.upstream_nocertify {
        return Err(eyre::eyre!(
            "--upstream.nocertify (uncertified dev sync) is not yet implemented"
        ));
    }

    // Trust anchor: the genesis validator committee (the MinPk consensus key
    // set), read from the follower's OWN genesis state. Consensus finality is a
    // multisig over these keys, so this set - not the VRF group key - is the
    // trust root, and it is already in genesis (the operator provides nothing).
    let follower_genesis_hash = genesis_hash(&node)?;
    let genesis_validators =
        validators::read_consensus_validators_at_block(&node.provider, follower_genesis_hash)
            .wrap_err("failed to read genesis validator set for the follower trust anchor")?;
    let anchor_participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        genesis_validators
            .public_keys
            .iter()
            .cloned()
            .try_collect()
            .map_err(|e| {
                eyre::eyre!("genesis validator set is not a valid participant set: {e:?}")
            })?;

    // Defence in depth for callers that construct the engine stack outside the
    // node binary. The binary already proves this equality before Reth launch;
    // repeat it here before certified sync so no alternate embedding can process
    // a protected block with a missing or divergent permanent key.
    let tee_probe = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    let tee_offer_public = tee_probe
        .tribute_offer_public_key()
        .await
        .wrap_err("failed to probe the upstream for TEE-chain status (follower prerequisites)")?;
    ensure!(
        !tee_offer_public.is_zero(),
        "selected upstream has no mandatory OST3 offer key"
    );
    let resident_offer = outbe_tee::resident_offer_public_key_v1()
        .wrap_err("failed to read the mandatory local enclave offer key")?;
    ensure!(
        resident_offer == tee_offer_public,
        "local enclave does not hold the selected chain's exact offer key; refusing certified sync (no recovery or fallback)"
    );

    info!(
        %upstream,
        anchor_validators = anchor_participants.len(),
        epoch_length,
        "follower mode (--upstream) selected; anchored on the genesis validator set"
    );

    let ocomp_storage_root = args
        .storage_dir
        .clone()
        .ok_or_else(|| eyre::eyre!("consensus storage_dir must be set before follower startup"))?;

    run_certified_follow_stack(
        ctx,
        anchor_participants,
        node,
        bridge,
        upstream,
        epoch_length,
        projection_readiness,
        ocomp_readiness,
        retained_tribute_writer,
        projection_retention_fence,
        retention_selector,
        finalized_ce_committer,
        ce_startup_recovery,
        ocomp_storage_root,
        follower_shutdown,
    )
    .await
}

/// Rebuild the exact parent-proof record a certified follower needs before
/// OCOMP retention may consume a finalized block.
///
/// Marshal has already verified the finalization certificate against `scheme`.
/// This seam additionally binds that verified certificate to the exact block
/// executed by the follower and to the historical committee snapshot committed
/// in the follower's own canonical state. A mismatch is node-fatal: substituting
/// either the current committee or a same-height block would make the locally
/// produced OCOMP input proof unverifiable.
pub(in crate::stack) fn build_certified_follower_parent_record(
    finalization: &outbe_consensus::marshal_types::Finalization,
    block: &outbe_consensus::block::ConsensusBlock,
    historical_snapshot: &outbe_consensus::proof::CommitteeSnapshot,
    scheme: &HybridScheme<MinSig>,
) -> Result<outbe_consensus::finalization::parent_cert_store::CertifiedParentProofRecord> {
    let finalized_hash = finalization.proposal.payload.0;
    ensure!(
        finalized_hash == block.block_hash(),
        "certified follower finalization payload {finalized_hash} differs from executed block {} at height {}",
        block.block_hash(),
        block.number(),
    );

    let finalized_epoch = finalization.proposal.round.epoch().get();
    let ordered_addresses: Vec<EthAddress> = historical_snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect();
    let record = outbe_consensus::finalization::resolver::build_finalization_record_from_recovered(
        finalized_epoch,
        finalization.proposal.round.view().get(),
        finalization.proposal.parent.get(),
        block.number(),
        finalized_hash,
        &ordered_addresses,
        &finalization.certificate,
        finalization.encode().into(),
        scheme,
    )?;
    let historical_hash = historical_snapshot.committee_set_hash_v2(finalized_epoch);
    ensure!(
        record.committee_set_hash == historical_hash,
        "certified follower verifier committee {} differs from historical committee snapshot {} at epoch {finalized_epoch}",
        record.committee_set_hash,
        historical_hash,
    );
    Ok(record)
}

async fn reconcile_certified_follower_height(
    node: &OutbeFullNode,
    marshal_mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    parent_cert_store: &outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    height: u64,
) -> Result<outbe_consensus::block::ConsensusBlock> {
    let finalization = marshal_mailbox
        .get_finalization(Height::new(height))
        .await
        .ok_or_else(|| {
            eyre::eyre!("marshal has no certified finalization at follower height {height}")
        })?;
    let block = marshal_mailbox
        .get_block(&finalization.proposal.payload)
        .await
        .ok_or_else(|| {
            eyre::eyre!(
                "marshal has no finalized block {} at follower height {height}",
                finalization.proposal.payload.0
            )
        })?;
    ensure!(
        block.number() == height,
        "marshal finalized block {} reports height {}, expected {height}",
        block.block_hash(),
        block.number(),
    );

    reconcile_certified_follower_record(
        node,
        certificate_scheme_provider,
        parent_cert_store,
        &finalization,
        &block,
    )?;
    Ok(block)
}

fn reconcile_certified_follower_record(
    node: &OutbeFullNode,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    parent_cert_store: &outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    finalization: &outbe_consensus::marshal_types::Finalization,
    block: &outbe_consensus::block::ConsensusBlock,
) -> Result<()> {
    use outbe_consensus::finalization::parent_cert_store::CertifiedParentProofStore as _;

    let epoch = finalization.proposal.round.epoch();
    let scheme = certificate_scheme_provider.scoped(epoch).ok_or_else(|| {
        eyre::eyre!(
            "certified follower has no verifier scheme for finalized epoch {}",
            epoch.get()
        )
    })?;
    let snapshot = validators::read_committee_snapshot_at_latest(&node.provider, epoch.get())?
        .ok_or_else(|| {
            eyre::eyre!(
                "certified follower has no historical committee snapshot for epoch {}",
                epoch.get()
            )
        })?;
    let height = block.number();
    let record =
        build_certified_follower_parent_record(finalization, block, &snapshot, scheme.as_ref())?;
    parent_cert_store
        .put_finalization(record)
        .wrap_err_with(|| format!("failed to persist follower finalization at height {height}"))?;
    parent_cert_store
        .prune_below_height(
            height.saturating_sub(outbe_consensus::finalization::actor::PARENT_CERT_KEEP_DEPTH),
        )
        .wrap_err("failed to prune follower finalized parent certificates")?;

    Ok(())
}

/// Genesis is the trusted follower anchor, not a block carrying a certified
/// marshal finalization. The executor still acknowledges height zero when it
/// observes the already-canonical genesis block, so finality observers must
/// ignore that one notification instead of asking marshal for an impossible
/// certificate.
pub(in crate::stack) const fn follower_height_has_certified_finalization(height: u64) -> bool {
    height > 0
}

/// The committee-chaining follower engine (transport A - upstream RPC, no
/// consensus P2P). Builds the same marshal + executor as the validator path,
/// feeds the marshal finalized blocks fetched from the upstream, and verifies
/// each against the per-epoch committee derived from the trusted anchor.
#[allow(clippy::too_many_arguments)]
async fn run_certified_follow_stack<E>(
    ctx: E,
    anchor_participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    upstream: String,
    epoch_length_blocks: u32,
    projection_readiness: ProjectionReadinessHandle,
    ocomp_readiness: Option<ProjectionReadinessHandle>,
    retained_tribute_writer: Arc<RetainedTributeWriter>,
    projection_retention_fence: Arc<ProjectionRetentionFence>,
    retention_selector: Arc<SharedOcompRetentionSelector>,
    finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    ce_startup_recovery: Arc<dyn CeStartupRecovery>,
    ocomp_storage_root: std::path::PathBuf,
    follower_shutdown: crate::follower_shutdown::FollowerDrain,
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
    use commonware_consensus::marshal;
    use commonware_cryptography::certificate::Verifier as _;
    use commonware_storage::archive::immutable;
    use outbe_consensus::follow::{
        run_follow_engine, CommitteeChain, FinalizedSource as _, FollowEngineConfig,
    };
    use outbe_consensus::hybrid::{HybridScheme, HybridSchemeProvider};
    use std::sync::{Arc, Mutex};

    // -- 0. Startup chain-state sources -----------------------------------
    let genesis_hash = genesis_hash(&node)?;
    let last_execution_height = node
        .provider
        .last_block_number()
        .map_err(|e| eyre::eyre!("failed to get last block number: {e}"))?;
    let initial_reth_forkchoice = read_reth_recovery_forkchoice(&node)?;

    // -- 1. Committee chain anchored on the trusted identity --------------
    // The marshal verifies finalization certs against THIS chain's per-epoch
    // verifier provider, so the provider clone we hand the marshal must share
    // state with the chain (HybridSchemeProvider is Arc-backed; `register`
    // through a clone is visible everywhere).
    // Genesis anchor: epoch 0, the genesis validator committee.
    let chain = CommitteeChain::new(Epoch::new(0), anchor_participants);
    let certificate_scheme_provider: HybridSchemeProvider<MinSig> = chain.scheme_provider().clone();
    let anchor_epoch = Epoch::new(chain.anchor_epoch());
    let chain = Arc::new(Mutex::new(chain));

    // -- 2. Page cache + marshal archives (mirrors run_consensus_stack) ---
    let page_cache = CacheRef::from_pooler(
        &ctx,
        nonzero_u16(4096, "page cache page size")?,
        nonzero_usize(config::PAGE_CACHE_SIZE / 4096, "PAGE_CACHE_SIZE / 4096")?,
    );

    let partition_prefix = "outbe-marshal".to_string();

    let mut finalizations_archive = immutable::Archive::init(
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

    let mut blocks_archive = immutable::Archive::init(
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

    // The follower marshal uses the boundary-aligned `FollowerEpocher`, whose
    // epoch boundaries match outbe's on-chain committee epochs (`[E*L+1,
    // (E+1)*L]`). The validator's `FixedEpocher` disagrees by one block at every
    // multiple of L, which would stall a resolver-only follower at boundary
    // blocks (see outbe_consensus::follow::epocher).
    let follower_rotation = DkgRotationParams::from_genesis(&node, epoch_length_blocks);
    let epocher = outbe_consensus::follow::FollowerEpocher::new(
        u64::from(epoch_length_blocks),
        follower_rotation.activation_grace_blocks,
    );
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

    let initial_archive_finalization_tip =
        marshal::store::Certificates::last_index(&finalizations_archive).map_or(0, Height::get);
    let initial_archive_block_tip =
        marshal::store::Blocks::last_index(&blocks_archive).map_or(0, Height::get);
    let (replay_suffix_lower, replay_suffix_upper) = certified_follower_replay_suffix_bounds(
        initial_archive_finalization_tip,
        initial_archive_block_tip,
        last_execution_height,
    );
    let upstream_client = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    let tip_client = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    if replay_suffix_upper > 0 {
        let (_, restored_finalizations, restored_blocks) =
            outbe_consensus::follow::engine::authenticate_and_reconcile_replay_suffix(
                &chain,
                &upstream_client,
                &epocher,
                anchor_epoch,
                Height::new(replay_suffix_lower),
                Height::new(replay_suffix_upper),
                finalizations_archive,
                blocks_archive,
            )
            .await
            .wrap_err("failed to authenticate and normalize follower replay suffix")?;
        finalizations_archive = restored_finalizations;
        blocks_archive = restored_blocks;
    }

    let archive_finalization_tip =
        marshal::store::Certificates::last_index(&finalizations_archive).map_or(0, Height::get);
    let archive_block_tip =
        marshal::store::Blocks::last_index(&blocks_archive).map_or(0, Height::get);
    ensure!(
        archive_finalization_tip == replay_suffix_upper && archive_block_tip == replay_suffix_upper,
        "certified follower replay normalization did not pair archive tips at height {replay_suffix_upper}: finalizations={archive_finalization_tip}, blocks={archive_block_tip}",
    );
    let preselected_anchor_height = archive_finalization_tip
        .min(archive_block_tip)
        .min(last_execution_height);
    let archived_finalization = if preselected_anchor_height == 0 {
        None
    } else {
        Some(
            marshal::store::Certificates::get(
                &finalizations_archive,
                commonware_storage::archive::Identifier::Index(preselected_anchor_height),
            )
            .await
            .wrap_err_with(|| {
                format!(
                    "failed to read archived follower finalization at height {preselected_anchor_height}"
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "follower finalization archive tip includes height {preselected_anchor_height} but its exact record is missing"
                )
            })?,
        )
    };
    let archived_block = if preselected_anchor_height == 0 {
        None
    } else {
        Some(
            marshal::store::Blocks::get(
                &blocks_archive,
                commonware_storage::archive::Identifier::Index(preselected_anchor_height),
            )
            .await
            .wrap_err_with(|| {
                format!(
                    "failed to read archived follower block at height {preselected_anchor_height}"
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "follower block archive tip includes height {preselected_anchor_height} but its exact record is missing"
                )
            })?,
        )
    };

    let marshal_genesis_anchor = genesis_consensus_block(&node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal::Config {
                provider: certificate_scheme_provider.clone(),
                epocher: epocher.clone(),
                start: marshal::Start::Genesis(marshal_genesis_anchor.clone()),
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
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt.height());
    let recovery_height =
        select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
            marshal_processed: last_consensus_finalized.get(),
            archive_finalization_tip,
            archive_block_tip,
            execution_tip: last_execution_height,
            reth_finalized: initial_reth_forkchoice
                .finalized
                .map_or(0, |checkpoint| checkpoint.block_number),
        })?;
    ensure!(
        recovery_height == preselected_anchor_height,
        "certified follower recovery height changed while Marshal archives were initialized: inspected {preselected_anchor_height}, selected {recovery_height}",
    );
    let recovery_hash = if recovery_height == 0 {
        genesis_hash
    } else {
        node.provider
            .block_hash(recovery_height)
            .map_err(|error| {
                eyre::eyre!(
                    "failed to read canonical Reth hash at recovery height {recovery_height}: {error}"
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "canonical Reth block is missing at recovery height {recovery_height}"
                )
            })?
    };

    // -- 3. Authenticate the immutable recovered anchor ------------------
    let local = crate::follow_transport::RethLocalBlockSource::new(node.clone());
    let recovery_anchor = if recovery_height == 0 {
        CertifiedFollowerRecoveryAnchor {
            checkpoint: ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis_hash,
            },
            finalization: None,
            block: marshal_genesis_anchor,
        }
    } else {
        let local_finalization = archived_finalization.as_ref().ok_or_else(|| {
            eyre::eyre!("missing local finalization for recovery height {recovery_height}")
        })?;
        let local_block = archived_block.as_ref().ok_or_else(|| {
            eyre::eyre!("missing local block for recovery height {recovery_height}")
        })?;
        let upstream = upstream_client
            .get_finalization(Height::new(recovery_height))
            .await
            .ok_or_else(|| {
                eyre::eyre!(
                    "upstream did not return exact recovery finalization at height {recovery_height}"
                )
            })?;
        validate_certified_follower_recovery_record(
            recovery_height,
            recovery_hash,
            local_finalization,
            local_block,
            &upstream.finalization,
            &upstream.block,
            &certificate_scheme_provider,
        )?
    };

    // -- 4. Closed startup barrier: FCU -> CE -> retention -> projections -
    let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
    let (execution_finalized_height_tx, mut execution_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (executor_actor, executor_mailbox) = ExecutorActor::new(
        ctx.child("executor"),
        engine_handle,
        genesis_hash,
        recovery_anchor.checkpoint.block_number,
        recovery_anchor.checkpoint.block_hash,
        projection_readiness.clone(),
        Some(execution_finalized_height_tx),
    );
    let fcu_provider_node = node.clone();
    confirm_recovered_forkchoice(
        ctx.child("recovered_forkchoice"),
        recovery_anchor.checkpoint,
        || executor_actor.replay_recovered_forkchoice_once(recovery_anchor.checkpoint),
        move || read_reth_recovery_forkchoice(&fcu_provider_node),
    )
    .await
    .wrap_err("failed to confirm recovered Reth forkchoice before follower startup")?;

    let recovered_ce_marker = ce_startup_recovery
        .recover_before_participation(recovery_anchor.checkpoint.block_number)
        .wrap_err("compressed-tree startup recovery failed before follower participation")?;

    let ocomp_fork_install =
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(node.chain_spec().as_ref())?;
    let install = ocomp_fork_install.as_ref();
    let parent_cert_dir = ocomp_storage_root.join("finalized_parent_certs");
    let finalized_parent_cert_store =
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore::open(
            &parent_cert_dir,
        )
        .wrap_err_with(|| {
            format!(
                "failed to open follower finalized parent certificate store at {}",
                parent_cert_dir.display()
            )
        })?;
    finalized_parent_cert_store
        .prune_above_height(recovery_anchor.checkpoint.block_number)
        .wrap_err("failed to prune follower parent certificates above recovery anchor")?;
    let pending_receipts_provider = node.provider.clone();
    let ocomp_proof_source = Arc::new(
        outbe_node::ocomp::retention::RethFinalizedInputProofSource::new(
            node.provider.clone(),
            finalized_parent_cert_store.clone(),
            move || {
                pending_receipts_provider
                    .pending_block_and_receipts()
                    .map(|pending| {
                        pending.map(|(block, receipts)| (B256::new(*block.hash()), receipts))
                    })
                    .map_err(|error| error.to_string())
            },
            install
                .request_profile
                .capacity_profile
                .result_deadline_blocks,
        ),
    );
    let ocomp_retention_coordinator = Arc::new(
        outbe_node::ocomp::retention::OcompRetentionCoordinator::open_with_retained_tributes_and_fence(
            ocomp_storage_root.join("ocomp_retention"),
            ocomp_proof_source,
            retained_tribute_writer,
            projection_retention_fence,
        ),
    );
    retention_selector
        .install(Arc::clone(&ocomp_retention_coordinator))
        .wrap_err("failed to install follower OCOMP retention selector")?;
    if let Some(finalization) = recovery_anchor.finalization.as_ref() {
        reconcile_certified_follower_record(
            &node,
            &certificate_scheme_provider,
            &finalized_parent_cert_store,
            finalization,
            &recovery_anchor.block,
        )
        .wrap_err("failed to reconcile recovered certified follower parent")?;
    }
    wait_for_recovered_projection(
        "offchain-data",
        projection_readiness.clone(),
        recovery_anchor.checkpoint,
    )
    .await?;
    if let Some(readiness) = ocomp_readiness.clone() {
        wait_for_recovered_projection("OCOMP", readiness, recovery_anchor.checkpoint).await?;
    }
    bridge.set_last_finalized_block_number(recovery_anchor.checkpoint.block_number);

    info!(
        marshal_processed = last_consensus_finalized.get(),
        recovery_height = recovery_anchor.checkpoint.block_number,
        recovery_hash = %recovery_anchor.checkpoint.block_hash,
        ce_marker_height = recovered_ce_marker.height,
        last_execution_height,
        "certified follower startup recovery barrier completed"
    );

    let executor_actor = executor_actor.with_finalized_ce_committer(finalized_ce_committer);
    let executor_actor = match ocomp_readiness {
        Some(readiness) => executor_actor.with_ocomp_readiness(readiness),
        None => executor_actor,
    };
    let Some(executor_reporter) = follower_shutdown.install(executor_mailbox)? else {
        // Shutdown won the startup race. No execution delivery was accepted.
        return Ok(());
    };
    let observer_ingress = executor_reporter.clone();
    let executor_handle = executor_actor.start(marshal_mailbox.clone(), last_consensus_finalized);

    // -- 4b. Serve `outbe_getFinalization`. The critical observer below owns
    // finality publication only after exact parent-proof persistence and OCOMP
    // retention both succeed. --------------------------------------
    spawn_finalization_drainer(&ctx, marshal_mailbox.clone(), bridge.clone());

    // -- 5. Assemble + run the follower engine ----------------------------
    let observer_mailbox = marshal_mailbox.clone();
    let observer_node = node.clone();
    let observer_schemes = certificate_scheme_provider.clone();
    let observer_store = finalized_parent_cert_store.clone();
    let observer_bridge = bridge.clone();
    let finality_observer = async move {
        let mut last_persisted = None;
        while let Some(height) = execution_finalized_height_rx.recv().await {
            if !follower_height_has_certified_finalization(height) {
                continue;
            }
            reconcile_certified_follower_height(
                &observer_node,
                &observer_mailbox,
                &observer_schemes,
                &observer_store,
                height,
            )
            .await?;
            observer_bridge.set_last_finalized_block_number(height);
            last_persisted = Some(height);
        }
        ensure!(
            observer_ingress.is_quiescing()?,
            "certified FullNode finality observer stopped before ingress quiesced"
        );
        if let Some(height) = last_persisted {
            let processed = observer_mailbox
                .get_processed_height()
                .await
                .ok_or_else(|| {
                    eyre::eyre!("marshal unavailable before follower proof drain completed")
                })?;
            ensure!(
                processed.get() >= height,
                "marshal has not acknowledged follower proof drain: processed {processed}, persisted {height}"
            );
        }
        Ok::<(), eyre::Report>(())
    };
    let follow_engine = run_follow_engine(
        ctx.child("follow_engine"),
        FollowEngineConfig {
            marshal_actor,
            marshal_mailbox,
            recovered_height: last_consensus_finalized,
            executor_reporter,
            upstream: upstream_client,
            local,
            tip: tip_client,
            epocher,
            chain,
            anchor_epoch,
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
        },
    );
    let executor_exit = async move {
        executor_handle
            .await
            .map_err(|error| eyre::eyre!("certified follower executor task failed: {error:?}"))?
    };
    // A clean marshal stop must not hide an executor error observed while its
    // mailbox drains, nor discard a queued finality reconciliation.
    let accepted_work = follower_shutdown.finish(executor_exit, finality_observer);
    futures::try_join!(follow_engine, accepted_work)?;
    Ok(())
}
