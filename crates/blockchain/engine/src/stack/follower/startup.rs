use super::*;
use crate::stack::recovery::forkchoice::RecoveredRethForkchoice;
use commonware_consensus::marshal;
use commonware_cryptography::certificate::Verifier as _;
use commonware_storage::archive::immutable;
use outbe_consensus::{
    block::ConsensusBlock,
    follow::{
        run_follow_engine, CommitteeChain, FinalizedSource as _, FollowEngineConfig,
        FollowerEpocher, SharedCommitteeChain,
    },
    marshal_types::{Finalization, FollowMarshalActor, MarshalMailbox},
};
use services::{FollowerConnection, FollowerStackServices};

pub(super) trait FollowerRuntime:
    BufferPooler
    + Clock
    + CryptoRng
    + Network
    + Resolver
    + Spawner
    + Storage
    + Metrics
    + Send
    + Sync
    + 'static
{
}
impl<E> FollowerRuntime for E where
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
        + 'static
{
}

pub(super) struct FollowerTrustAnchor {
    pub(super) participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(super) epoch_length_blocks: u32,
    pub(super) storage_root: std::path::PathBuf,
}
struct StartupChainState {
    genesis_hash: B256,
    last_execution_height: u64,
    initial_reth_forkchoice: RecoveredRethForkchoice,
}
struct FollowerArchives<E: FollowerRuntime> {
    finalizations_archive: immutable::Archive<E, Digest, Finalization>,
    blocks_archive: immutable::Archive<E, Digest, ConsensusBlock>,
    page_cache: CacheRef,
    partition_prefix: String,
}
struct ReplayAuthority<'a> {
    chain: &'a SharedCommitteeChain,
    source: &'a crate::follow_transport::UpstreamRpcClient,
    epocher: &'a FollowerEpocher,
    anchor_epoch: Epoch,
}
struct ArchiveAnchor {
    recovery_archive_tip: u64,
    preselected_anchor_height: u64,
    archived_finalization: Option<Finalization>,
    archived_block: Option<ConsensusBlock>,
}
struct MarshalAuthority<'a> {
    provider: &'a HybridSchemeProvider<MinSig>,
    epocher: &'a FollowerEpocher,
    view_retention_timeout: u64,
}
struct InitializedMarshal<E: FollowerRuntime> {
    actor: FollowMarshalActor<E>,
    mailbox: MarshalMailbox,
    processed_height: Height,
    genesis_anchor: ConsensusBlock,
}
struct RecoveryAuthority<'a> {
    provider: &'a HybridSchemeProvider<MinSig>,
    upstream: &'a crate::follow_transport::UpstreamRpcClient,
    epocher: &'a FollowerEpocher,
}
struct RecoveredFollowerHistory<E: FollowerRuntime> {
    marshal_actor: FollowMarshalActor<E>,
    marshal_mailbox: MarshalMailbox,
    last_consensus_finalized: Height,
    recovery_anchor: CertifiedFollowerRecoveryAnchor,
    certificate_scheme_provider: HybridSchemeProvider<MinSig>,
    chain: SharedCommitteeChain,
    anchor_epoch: Epoch,
    epocher: FollowerEpocher,
    upstream_client: crate::follow_transport::UpstreamRpcClient,
    tip_client: crate::follow_transport::UpstreamRpcClient,
    local: crate::follow_transport::RethLocalBlockSource,
}
struct HistoryBootstrap<'a, E: FollowerRuntime> {
    ctx: &'a E,
    node: &'a OutbeFullNode,
    startup: &'a StartupChainState,
    upstream: &'a str,
    participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    epoch_length_blocks: u32,
}
pub(super) async fn run_certified_follow_stack<E>(
    ctx: E,
    connection: FollowerConnection,
    trust: FollowerTrustAnchor,
    services: FollowerStackServices,
) -> Result<()>
where
    E: FollowerRuntime,
{
    let FollowerConnection {
        node,
        bridge,
        upstream,
    } = connection;
    let FollowerTrustAnchor {
        participants,
        epoch_length_blocks,
        storage_root: ocomp_storage_root,
    } = trust;
    let startup = read_startup_chain_state(&node)?;
    let history = HistoryBootstrap {
        ctx: &ctx,
        node: &node,
        startup: &startup,
        upstream: &upstream,
        participants,
        epoch_length_blocks,
    }
    .recover()
    .await?;
    ExecutionBootstrap {
        ctx,
        node,
        bridge,
        history,
        startup,
        services,
        storage_root: ocomp_storage_root,
    }
    .run()
    .await
}

fn read_startup_chain_state(node: &OutbeFullNode) -> Result<StartupChainState> {
    // -- 0. Startup chain-state sources -----------------------------------
    let genesis_hash = genesis_hash(node)?;
    let last_execution_height = node
        .provider
        .last_block_number()
        .map_err(|e| eyre::eyre!("failed to get last block number: {e}"))?;
    let initial_reth_forkchoice =
        read_reth_recovery_forkchoice(&node.provider.canonical_in_memory_state())?;

    Ok(StartupChainState {
        genesis_hash,
        last_execution_height,
        initial_reth_forkchoice,
    })
}
async fn initialize_archives<E: FollowerRuntime>(ctx: &E) -> Result<FollowerArchives<E>> {
    // -- 2. Page cache + marshal archives (mirrors run_consensus_stack) ---
    let page_cache = CacheRef::from_pooler(
        ctx,
        nonzero_u16(4096, "page cache page size")?,
        nonzero_usize(config::PAGE_CACHE_SIZE / 4096, "PAGE_CACHE_SIZE / 4096")?,
    );

    let partition_prefix = "outbe-marshal".to_string();

    let finalizations_archive = immutable::Archive::init(
        ctx.child("marshal_finalizations"),
        marshal_archive::archive_config(
            &partition_prefix,
            marshal_archive::ArchiveKind::Finalizations,
            &page_cache,
            HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
        )?,
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        marshal_archive::archive_config(
            &partition_prefix,
            marshal_archive::ArchiveKind::Blocks,
            &page_cache,
            (),
        )?,
    )
    .await
    .wrap_err("failed to initialize blocks archive")?;

    Ok(FollowerArchives {
        finalizations_archive,
        blocks_archive,
        page_cache,
        partition_prefix,
    })
}
async fn normalize_replay_suffix<E: FollowerRuntime>(
    archives: FollowerArchives<E>,
    authority: ReplayAuthority<'_>,
    last_execution_height: u64,
) -> Result<(FollowerArchives<E>, u64)> {
    let FollowerArchives {
        mut finalizations_archive,
        mut blocks_archive,
        page_cache,
        partition_prefix,
    } = archives;
    let ReplayAuthority {
        chain,
        source: upstream_client,
        epocher,
        anchor_epoch,
    } = authority;
    let initial_archive_finalization_tip =
        marshal::store::Certificates::last_index(&finalizations_archive).map_or(0, Height::get);
    let initial_archive_block_tip =
        marshal::store::Blocks::last_index(&blocks_archive).map_or(0, Height::get);
    let (replay_suffix_lower, replay_suffix_upper) = certified_follower_replay_suffix_bounds(
        initial_archive_finalization_tip,
        initial_archive_block_tip,
        last_execution_height,
    );
    if replay_suffix_upper > 0 {
        let (_, restored_finalizations, restored_blocks) =
            outbe_consensus::follow::engine::authenticate_and_reconcile_replay_suffix(
                outbe_consensus::follow::engine::ReplayAuthority {
                    chain,
                    source: upstream_client,
                    epocher,
                },
                outbe_consensus::follow::engine::ReplayWindow {
                    anchor_epoch,
                    lower: Height::new(replay_suffix_lower),
                    upper: Height::new(replay_suffix_upper),
                },
                outbe_consensus::follow::engine::ReplayArchives::new(
                    finalizations_archive,
                    blocks_archive,
                ),
            )
            .await
            .wrap_err("failed to authenticate and normalize follower replay suffix")?;
        finalizations_archive = restored_finalizations;
        blocks_archive = restored_blocks;
    }

    Ok((
        FollowerArchives {
            finalizations_archive,
            blocks_archive,
            page_cache,
            partition_prefix,
        },
        replay_suffix_upper,
    ))
}
async fn inspect_archive_anchor<E: FollowerRuntime>(
    archives: &FollowerArchives<E>,
    replay_suffix_upper: u64,
    last_execution_height: u64,
) -> Result<ArchiveAnchor> {
    let finalizations_archive = &archives.finalizations_archive;
    let blocks_archive = &archives.blocks_archive;
    let archive_finalization_tip =
        marshal::store::Certificates::last_index(finalizations_archive).map_or(0, Height::get);
    let archive_block_tip =
        marshal::store::Blocks::last_index(blocks_archive).map_or(0, Height::get);
    ensure!(
        archive_finalization_tip == archive_block_tip
            && archive_finalization_tip >= replay_suffix_upper
            && archive_finalization_tip <= replay_suffix_upper.saturating_add(64),
        "certified follower replay normalization did not pair bounded archive tips from height {replay_suffix_upper}: finalizations={archive_finalization_tip}, blocks={archive_block_tip}",
    );
    let recovery_archive_tip = archive_finalization_tip.min(replay_suffix_upper);
    let preselected_anchor_height = recovery_archive_tip.min(last_execution_height);
    let archived_finalization = if preselected_anchor_height == 0 {
        None
    } else {
        marshal::store::Certificates::get(
                finalizations_archive,
                commonware_storage::archive::Identifier::Index(preselected_anchor_height),
            )
            .await
            .wrap_err_with(|| {
                format!(
                    "failed to read archived follower finalization at height {preselected_anchor_height}"
                )
            })?
    };
    let archived_block = if preselected_anchor_height == 0 {
        None
    } else {
        Some(
            marshal::store::Blocks::get(
                blocks_archive,
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

    Ok(ArchiveAnchor {
        recovery_archive_tip,
        preselected_anchor_height,
        archived_finalization,
        archived_block,
    })
}
async fn initialize_marshal<E: FollowerRuntime>(
    ctx: &E,
    node: &OutbeFullNode,
    archives: FollowerArchives<E>,
    authority: MarshalAuthority<'_>,
) -> Result<InitializedMarshal<E>> {
    let FollowerArchives {
        finalizations_archive,
        blocks_archive,
        page_cache,
        partition_prefix,
    } = archives;
    let MarshalAuthority {
        provider: certificate_scheme_provider,
        epocher,
        view_retention_timeout,
    } = authority;
    let marshal_genesis_anchor = genesis_consensus_block(node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal_archive::marshal_config(
                certificate_scheme_provider.clone(),
                marshal_archive::MarshalStart {
                    epocher: epocher.clone(),
                    genesis: marshal_genesis_anchor.clone(),
                },
                marshal_archive::MarshalSettings {
                    partition_prefix: &partition_prefix,
                    page_cache: &page_cache,
                    view_retention_timeout,
                },
            )?,
        )
        .await;
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt.height());
    Ok(InitializedMarshal {
        actor: marshal_actor,
        mailbox: marshal_mailbox,
        processed_height: last_consensus_finalized,
        genesis_anchor: marshal_genesis_anchor,
    })
}
fn recovered_checkpoint(
    node: &OutbeFullNode,
    startup: &StartupChainState,
    inspected: &ArchiveAnchor,
    last_consensus_finalized: Height,
) -> Result<ProjectionCheckpoint> {
    let StartupChainState {
        genesis_hash,
        last_execution_height,
        initial_reth_forkchoice,
    } = *startup;
    let recovery_archive_tip = inspected.recovery_archive_tip;
    let preselected_anchor_height = inspected.preselected_anchor_height;
    let recovery_height =
        select_certified_follower_recovery_height(CertifiedFollowerRecoveryFloors {
            marshal_processed: last_consensus_finalized.get(),
            archive_finalization_tip: recovery_archive_tip,
            archive_block_tip: recovery_archive_tip,
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

    Ok(ProjectionCheckpoint {
        block_number: recovery_height,
        block_hash: recovery_hash,
    })
}
impl RecoveryAuthority<'_> {
    async fn authenticate(
        self,
        checkpoint: ProjectionCheckpoint,
        inspected: &ArchiveAnchor,
        marshal_genesis_anchor: ConsensusBlock,
    ) -> Result<CertifiedFollowerRecoveryAnchor> {
        let Self {
            provider: certificate_scheme_provider,
            upstream: upstream_client,
            epocher,
        } = self;
        let recovery_height = checkpoint.block_number;
        let recovery_hash = checkpoint.block_hash;
        let archived_block = &inspected.archived_block;
        let archived_finalization = &inspected.archived_finalization;
        let recovery_anchor = if recovery_height == 0 {
            CertifiedFollowerRecoveryAnchor {
                checkpoint: ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: recovery_hash,
                },
                finalization: None,
                block: marshal_genesis_anchor,
            }
        } else {
            let local_block = archived_block.as_ref().ok_or_else(|| {
                eyre::eyre!("missing local block for recovery height {recovery_height}")
            })?;
            let upstream = upstream_client
            .get_finality_proof(Height::new(recovery_height))
            .await
            .ok_or_else(|| {
                eyre::eyre!(
                    "upstream did not return exact recovery finalization at height {recovery_height}"
                )
            })?;
            validate_ancestor_follower_recovery_record(
                crate::stack::recovery::anchor::FollowerRecoveryBlock {
                    checkpoint: ProjectionCheckpoint {
                        block_number: recovery_height,
                        block_hash: recovery_hash,
                    },
                    block: local_block,
                },
                archived_finalization.as_ref(),
                &upstream,
                certificate_scheme_provider,
                epocher,
            )?
        };

        Ok(recovery_anchor)
    }
}
impl<E: FollowerRuntime> HistoryBootstrap<'_, E> {
    async fn restore_committee(
        &self,
        archives: &FollowerArchives<E>,
    ) -> Result<super::local_anchor::RestoredCommittee> {
        let follower_rotation =
            DkgRotationParams::from_genesis(self.node, self.epoch_length_blocks);
        let epoch_length = u64::from(self.epoch_length_blocks);
        let activation_grace = follower_rotation.activation_grace_blocks;
        let (floor, _) = certified_follower_replay_suffix_bounds(
            marshal::store::Certificates::last_index(&archives.finalizations_archive)
                .map_or(0, Height::get),
            marshal::store::Blocks::last_index(&archives.blocks_archive).map_or(0, Height::get),
            self.startup.last_execution_height,
        );
        let restored = super::local_anchor::LocalFinalizedHistory {
            certificates: &archives.finalizations_archive,
            blocks: &archives.blocks_archive,
            canonical_hash: |height| self.node.provider.block_hash(height).map_err(Into::into),
            floor,
            epoch_length,
            activation_grace,
        }
        .restore()
        .await?;
        Ok(match restored {
            Some(restored) => {
                let epoch = Epoch::new(restored.chain.anchor_epoch());
                let activation = restored
                    .epocher
                    .activation_height(epoch)
                    .ok_or_else(|| eyre::eyre!("restored follower boundary is missing"))?;
                info!(
                    anchor_epoch = epoch.get(),
                    anchor_height = activation.get(),
                    "follower restored committee from local finalized history"
                );
                restored
            }
            None => super::local_anchor::RestoredCommittee {
                chain: CommitteeChain::new(Epoch::new(0), self.participants.clone()),
                epocher: FollowerEpocher::new(epoch_length, activation_grace),
            },
        })
    }

    async fn recover(self) -> Result<RecoveredFollowerHistory<E>> {
        let archives = initialize_archives(self.ctx).await?;
        let super::local_anchor::RestoredCommittee { chain, epocher } =
            self.restore_committee(&archives).await?;
        let Self {
            ctx,
            node,
            startup,
            upstream,
            ..
        } = self;
        // All follower paths use the same verifier provider and observed boundaries.
        let certificate_scheme_provider = chain.scheme_provider().clone();
        let anchor_epoch = Epoch::new(chain.anchor_epoch());
        let chain = SharedCommitteeChain::new(chain);
        let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
            .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
            .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

        let upstream_client = crate::follow_transport::UpstreamRpcClient::new(upstream)?;
        let tip_client = crate::follow_transport::UpstreamRpcClient::new(upstream)?;
        let (archives, upper) = normalize_replay_suffix(
            archives,
            ReplayAuthority {
                chain: &chain,
                source: &upstream_client,
                epocher: &epocher,
                anchor_epoch,
            },
            startup.last_execution_height,
        )
        .await?;
        let inspected =
            inspect_archive_anchor(&archives, upper, startup.last_execution_height).await?;
        let initialized = initialize_marshal(
            ctx,
            node,
            archives,
            MarshalAuthority {
                provider: &certificate_scheme_provider,
                epocher: &epocher,
                view_retention_timeout,
            },
        )
        .await?;
        let checkpoint =
            recovered_checkpoint(node, startup, &inspected, initialized.processed_height)?;
        let local = crate::follow_transport::RethLocalBlockSource::new(node.clone());
        let recovery_anchor = RecoveryAuthority {
            provider: &certificate_scheme_provider,
            upstream: &upstream_client,
            epocher: &epocher,
        }
        .authenticate(checkpoint, &inspected, initialized.genesis_anchor)
        .await?;
        Ok(RecoveredFollowerHistory {
            marshal_actor: initialized.actor,
            marshal_mailbox: initialized.mailbox,
            last_consensus_finalized: initialized.processed_height,
            recovery_anchor,
            certificate_scheme_provider,
            chain,
            anchor_epoch,
            epocher,
            upstream_client,
            tip_client,
            local,
        })
    }
}

struct ExecutionBootstrap<E: FollowerRuntime> {
    ctx: E,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    history: RecoveredFollowerHistory<E>,
    startup: StartupChainState,
    services: FollowerStackServices,
    storage_root: std::path::PathBuf,
}
struct ConfirmedExecutor<E: FollowerRuntime> {
    actor: ExecutorActor<E>,
    mailbox: outbe_consensus::executor::Mailbox,
    finalized_heights: tokio::sync::mpsc::UnboundedReceiver<u64>,
}
struct RetentionBootstrap<'a> {
    node: &'a OutbeFullNode,
    ocomp_storage_root: &'a std::path::Path,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
}
struct RetentionServices<'a> {
    retained_tribute_writer: Arc<RetainedTributeWriter>,
    projection_retention_fence: Arc<ProjectionRetentionFence>,
    retention_selector: &'a Arc<SharedOcompRetentionSelector>,
}
struct ProjectionBootstrap<'a> {
    bridge: &'a ConsensusExecutionBridge,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    last_consensus_finalized: Height,
    last_execution_height: u64,
}
struct FollowerReadiness<'a> {
    projection: &'a ProjectionReadinessHandle,
    ocomp: &'a Option<ProjectionReadinessHandle>,
}
impl<E: FollowerRuntime> ExecutionBootstrap<E> {
    async fn confirmed_executor(&self) -> Result<ConfirmedExecutor<E>> {
        let ctx = &self.ctx;
        let node = &self.node;
        let genesis_hash = self.startup.genesis_hash;
        let recovery_anchor = &self.history.recovery_anchor;
        let projection_readiness = &self.services.projection_readiness;
        let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
        let (execution_finalized_height_tx, execution_finalized_height_rx) =
            tokio::sync::mpsc::unbounded_channel::<u64>();
        let (executor_actor, executor_mailbox) = ExecutorActor::new(
            ctx.child("executor"),
            engine_handle,
            outbe_consensus::executor::actor::RecoveredFinalizedState {
                genesis_hash,
                last_finalized_height: recovery_anchor.checkpoint.block_number,
                last_finalized_hash: recovery_anchor.checkpoint.block_hash,
            },
            projection_readiness.clone(),
            Some(execution_finalized_height_tx),
        );
        let fcu_provider_node = node.clone();
        confirm_recovered_forkchoice(
            ctx.child("recovered_forkchoice"),
            recovery_anchor.checkpoint,
            || executor_actor.replay_recovered_forkchoice_once(recovery_anchor.checkpoint),
            move || {
                read_reth_recovery_forkchoice(
                    &fcu_provider_node.provider.canonical_in_memory_state(),
                )
            },
        )
        .await
        .wrap_err("failed to confirm recovered Reth forkchoice before follower startup")?;

        Ok(ConfirmedExecutor {
            actor: executor_actor,
            mailbox: executor_mailbox,
            finalized_heights: execution_finalized_height_rx,
        })
    }
    async fn run(self) -> Result<()> {
        let ConfirmedExecutor {
            actor: executor_actor,
            mailbox: executor_mailbox,
            finalized_heights: execution_finalized_height_rx,
        } = self.confirmed_executor().await?;
        let FollowerStackServices {
            projection_readiness,
            ocomp_readiness,
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector,
            finalized_ce_committer,
            ce_startup_recovery,
            follower_shutdown,
        } = self.services;
        let recovery_anchor = &self.history.recovery_anchor;
        let recovered_ce_marker = ce_startup_recovery
            .recover_before_participation(recovery_anchor.checkpoint.block_number)
            .wrap_err("compressed-tree startup recovery failed before follower participation")?;
        let finalized_parent_cert_store = RetentionBootstrap {
            node: &self.node,
            ocomp_storage_root: &self.storage_root,
            recovery_anchor,
            certificate_scheme_provider: &self.history.certificate_scheme_provider,
        }
        .install(RetentionServices {
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector: &retention_selector,
        })?;
        ProjectionBootstrap {
            bridge: &self.bridge,
            recovery_anchor,
            last_consensus_finalized: self.history.last_consensus_finalized,
            last_execution_height: self.startup.last_execution_height,
        }
        .wait(
            FollowerReadiness {
                projection: &projection_readiness,
                ocomp: &ocomp_readiness,
            },
            recovered_ce_marker.height,
        )
        .await?;
        let marshal_mailbox = &self.history.marshal_mailbox;
        let last_consensus_finalized = self.history.last_consensus_finalized;
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
        let executor_handle =
            executor_actor.start(marshal_mailbox.clone(), last_consensus_finalized);

        LiveFollower {
            ctx: self.ctx,
            node: self.node,
            bridge: self.bridge,
            history: self.history,
            finalized_parent_cert_store,
            executor_reporter,
            observer_ingress,
            executor_handle,
            execution_finalized_height_rx,
            follower_shutdown,
            _projection_readiness: projection_readiness,
            _retention_selector: retention_selector,
            _ce_startup_recovery: ce_startup_recovery,
        }
        .run()
        .await
    }
}
impl RetentionBootstrap<'_> {
    fn install(
        self,
        services: RetentionServices<'_>,
    ) -> Result<outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore> {
        let Self {
            node,
            ocomp_storage_root,
            recovery_anchor,
            certificate_scheme_provider,
        } = self;
        let RetentionServices {
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector,
        } = services;
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(node.chain_spec().as_ref())?;
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
        let ocomp_proof_source = Arc::new(
            outbe_node::ocomp::retention::RethFinalizedInputProofSource::new(
                node.provider.clone(),
                finalized_parent_cert_store.clone(),
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
            FollowerProofPersistence {
                node,
                certificate_scheme_provider,
                parent_cert_store: &finalized_parent_cert_store,
            }
            .reconcile_record(finalization, &recovery_anchor.block)
            .wrap_err("failed to reconcile recovered certified follower parent")?;
        }
        Ok(finalized_parent_cert_store)
    }
}
impl ProjectionBootstrap<'_> {
    async fn wait(self, readiness: FollowerReadiness<'_>, ce_marker_height: u64) -> Result<()> {
        let Self {
            bridge,
            recovery_anchor,
            last_consensus_finalized,
            last_execution_height,
        } = self;
        let FollowerReadiness {
            projection: projection_readiness,
            ocomp: ocomp_readiness,
        } = readiness;
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
            ce_marker_height,
            last_execution_height,
            "certified follower startup recovery barrier completed"
        );

        Ok(())
    }
}
struct LiveFollower<E: FollowerRuntime> {
    ctx: E,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    history: RecoveredFollowerHistory<E>,
    finalized_parent_cert_store:
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    executor_reporter: crate::follower_shutdown::FollowerReporter,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    executor_handle: commonware_runtime::Handle<Result<()>>,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    follower_shutdown: crate::follower_shutdown::FollowerDrain,
    // The original startup held these owners until execution/proof draining completed.
    _projection_readiness: ProjectionReadinessHandle,
    _retention_selector: Arc<SharedOcompRetentionSelector>,
    _ce_startup_recovery: Arc<dyn CeStartupRecovery>,
}
impl<E: FollowerRuntime> LiveFollower<E> {
    async fn run(self) -> Result<()> {
        let Self {
            ctx,
            node,
            bridge,
            history,
            finalized_parent_cert_store,
            executor_reporter,
            observer_ingress,
            executor_handle,
            execution_finalized_height_rx,
            follower_shutdown,
            _projection_readiness,
            _retention_selector,
            _ce_startup_recovery,
        } = self;
        let RecoveredFollowerHistory {
            marshal_actor,
            marshal_mailbox,
            last_consensus_finalized,
            certificate_scheme_provider,
            chain,
            anchor_epoch,
            epocher,
            upstream_client,
            tip_client,
            local,
            ..
        } = history;
        // -- 4b. Serve `outbe_getFinalization`. The critical observer below owns
        // finality publication only after exact parent-proof persistence and OCOMP
        // retention both succeed. --------------------------------------
        spawn_finalization_drainer(
            &ctx,
            marshal_mailbox.clone(),
            bridge.clone(),
            finalized_parent_cert_store.clone(),
        );

        // -- 5. Assemble + run the follower engine ----------------------------
        let finality_observer = FinalityObserver {
            observer_mailbox: marshal_mailbox.clone(),
            observer_node: node.clone(),
            observer_schemes: certificate_scheme_provider.clone(),
            observer_store: finalized_parent_cert_store.clone(),
            observer_bridge: bridge.clone(),
            observer_ingress,
            execution_finalized_height_rx,
        }
        .run();
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
            executor_handle.await.map_err(|error| {
                eyre::eyre!("certified follower executor task failed: {error:?}")
            })?
        };
        // A clean marshal stop must not hide an executor error observed while its
        // mailbox drains, nor discard a queued finality reconciliation.
        let accepted_work = follower_shutdown.finish(executor_exit, finality_observer);
        futures::try_join!(follow_engine, accepted_work)?;
        Ok(())
    }
}
struct FinalityObserver {
    observer_mailbox: MarshalMailbox,
    observer_node: OutbeFullNode,
    observer_schemes: HybridSchemeProvider<MinSig>,
    observer_store: outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    observer_bridge: ConsensusExecutionBridge,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
}
impl FinalityObserver {
    async fn run(self) -> Result<()> {
        let Self {
            observer_mailbox,
            observer_node,
            observer_schemes,
            observer_store,
            observer_bridge,
            observer_ingress,
            mut execution_finalized_height_rx,
        } = self;

        let mut last_persisted = None;
        while let Some(height) = execution_finalized_height_rx.recv().await {
            if !follower_height_has_certified_finalization(height) {
                continue;
            }
            if observer_mailbox
                .get_finalization(Height::new(height))
                .await
                .is_none()
            {
                ensure!(
                    observer_mailbox
                        .get_info(Height::new(height))
                        .await
                        .is_some(),
                    "marshal lost finalized follower block {height} before proof drain"
                );
                continue;
            }
            FollowerProofPersistence {
                node: &observer_node,
                certificate_scheme_provider: &observer_schemes,
                parent_cert_store: &observer_store,
            }
            .reconcile_height(&observer_mailbox, height)
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
    }
}
