//! Embedded OCOMP observer and local FullNode readiness gate.
//!
//! Finalized chain state remains the only authority. ExEx notifications are
//! drained for Reth backpressure, while the provider's finalized head selects
//! the exact canonical range processed here.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{DirBuilder, File, OpenOptions},
    io::{Read as _, Write},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use alloy_consensus::{BlockHeader as _, Transaction as _, TxReceipt as _};
use alloy_primitives::{Address, Sealable as _, B256, U256};
use alloy_sol_types::SolEvent as _;
use eyre::{bail, Context as _};
use futures::StreamExt as _;
use outbe_metadosis::precompile::IMetadosis;
use outbe_ocomp::payout_submitter::PayoutTickOutcomeV1;
use outbe_ocomp::{
    bundle::PinnedProtocolBundle,
    embedded::{
        EmbeddedJobActionV1, EmbeddedJobEventV1, EmbeddedJobGenerationV1, EmbeddedJobStateV1,
        EmbeddedOcompJobsV1, EmbeddedOcompModeV1, EmbeddedTerminalReasonV1,
    },
    embedded_checkpoint::{OcompExExCheckpointStoreV1, OcompExExCheckpointV1},
    embedded_runtime::{
        EmbeddedComputeOutcomeV1, EmbeddedMaterializationOutcomeV1, EmbeddedNodePolicyV1,
        EmbeddedOcompBundleConfigV1, EmbeddedOcompDomainConfigV1, EmbeddedOcompDomainV1,
        EmbeddedOcompRuntimeErrorV1, EmbeddedPayoutOutcomeV1, EmbeddedVoteOutcomeV1,
    },
    supervisor::DiscoveryRecord,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
    local_control::EndpointIdentity,
    profile::poc_schema_limits,
    result::LysisResultV1,
    state::{OcompJobRecordV1, OcompJobStatus},
    vote::{decode_submit_lysis_result, ResultVoteV1},
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    projection::{
        ProjectionCheckpoint, ProjectionFailure, ProjectionFailureClass,
        ProjectionReadinessPublisher, ProjectionStatus,
    },
    storage::readonly::{ReadOnlyStorageProvider, StorageReader},
    storage::StorageHandle,
    OutbeReceipt,
};
use reth_ethereum::exex::{ExExContext, ExExEvent, ExExHead};
use reth_node_builder::FullNodeComponents;
use reth_primitives_traits::{Block as _, BlockBody as _};
use reth_provider::{
    BlockHashReader, BlockIdReader, BlockNumReader, BlockReader, ReceiptProvider, StateProvider,
    StateProviderFactory,
};
use tracing::{error, info, warn};

/// Days each embedded payout tick looks back over.
const PAYOUT_LOOKBACK_DAYS: u32 = 30;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct OcompExExBundleConfigV1 {
    pub worker_address: std::net::SocketAddr,
    pub identity: EndpointIdentity,
    pub protocol_bundle: PinnedProtocolBundle,
}

#[derive(Clone)]
pub struct OcompExExConfigV1 {
    pub domain_root: PathBuf,
    pub bundles: Vec<OcompExExBundleConfigV1>,
    pub policy: EmbeddedNodePolicyV1,
    pub validator_rpc_url: Option<String>,
    pub chain_id: u64,
    pub genesis_hash: B256,
}

#[derive(Clone, Debug)]
pub struct OcompExExExitV1 {
    pub failure: ProjectionFailure,
}

#[derive(Clone, Copy, Debug)]
struct RequestLocatorV1 {
    intent_id: B256,
    wwd: u32,
    pending_nonce: u64,
    attempt: u32,
    activation_preconditions_hash: B256,
    block_number: u64,
    block_hash: B256,
    state_root: B256,
}

struct RuntimeJobV1 {
    record: DiscoveryRecord,
    generation: EmbeddedJobGenerationV1,
    cancelled: Arc<AtomicBool>,
    compute_started: bool,
    vote_eligibility: LocalVoteEligibilityV1,
    vote_started: bool,
    canonical_result: Option<LysisResultV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalVoteEligibilityV1 {
    Pending,
    Eligible,
    NotMember,
}

fn advance_vote_eligibility(
    current: LocalVoteEligibilityV1,
    observed: outbe_node::ocomp::retention::OcompSnapshotEligibilityV1,
) -> eyre::Result<LocalVoteEligibilityV1> {
    if current != LocalVoteEligibilityV1::Pending {
        return Ok(current);
    }
    match observed {
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Eligible => {
            Ok(LocalVoteEligibilityV1::Eligible)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::NotMember => {
            Ok(LocalVoteEligibilityV1::NotMember)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Unavailable { .. } => {
            Ok(LocalVoteEligibilityV1::Pending)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Corrupt { detail } => {
            bail!("pinned OCOMP vote membership is corrupt: {detail}")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MaterializationAttemptKeyV1 {
    queue_sequence: u64,
    first_nod_ordinal: u32,
}

fn bound_materialization_attempts(
    attempts: &mut BTreeMap<MaterializationAttemptKeyV1, u64>,
    current: Option<MaterializationAttemptKeyV1>,
) {
    attempts.retain(|key, _| Some(*key) == current);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalizedHeadPhaseV1 {
    StartupRecovery,
    Running,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CanonicalJobDispositionV1 {
    AwaitingFinality,
    FinalizedAwaitingOpen,
    VotingOpen,
    Completed,
    Closed {
        reason: EmbeddedTerminalReasonV1,
        has_finalized_job: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncOutcomeProjectionV1 {
    Active,
    CheckpointPruned,
}

fn classify_async_outcome_projection(
    runtime_generation: Option<EmbeddedJobGenerationV1>,
    reducer_generation: Option<EmbeddedJobGenerationV1>,
) -> eyre::Result<AsyncOutcomeProjectionV1> {
    match (runtime_generation, reducer_generation) {
        (Some(runtime), Some(reducer)) if runtime == reducer => {
            Ok(AsyncOutcomeProjectionV1::Active)
        }
        (None, None) => Ok(AsyncOutcomeProjectionV1::CheckpointPruned),
        (Some(_), Some(_)) => bail!("OCOMP runtime and reducer generations disagree"),
        (Some(_), None) | (None, Some(_)) => {
            bail!("OCOMP runtime and reducer projections disagree")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalResultRestorePolicyV1 {
    Never,
    BeforeCompute,
    AfterCanonicalCompleted,
}

fn local_result_restore_policy(
    disposition: CanonicalJobDispositionV1,
    policy: EmbeddedNodePolicyV1,
    discovered: bool,
    eligibility_became_available: bool,
    compute_started: bool,
) -> LocalResultRestorePolicyV1 {
    match disposition {
        CanonicalJobDispositionV1::VotingOpen
            if discovered || eligibility_became_available || !compute_started =>
        {
            LocalResultRestorePolicyV1::BeforeCompute
        }
        CanonicalJobDispositionV1::Completed
            if policy == EmbeddedNodePolicyV1::FullNode && (discovered || !compute_started) =>
        {
            LocalResultRestorePolicyV1::AfterCanonicalCompleted
        }
        CanonicalJobDispositionV1::AwaitingFinality
        | CanonicalJobDispositionV1::FinalizedAwaitingOpen
        | CanonicalJobDispositionV1::VotingOpen
        | CanonicalJobDispositionV1::Completed
        | CanonicalJobDispositionV1::Closed { .. } => LocalResultRestorePolicyV1::Never,
    }
}

fn classify_canonical_job(
    status: OcompJobStatus,
    has_finalized_job: bool,
) -> eyre::Result<CanonicalJobDispositionV1> {
    Ok(match status {
        OcompJobStatus::AwaitingFinality if !has_finalized_job => {
            CanonicalJobDispositionV1::AwaitingFinality
        }
        OcompJobStatus::AwaitingFinality => CanonicalJobDispositionV1::FinalizedAwaitingOpen,
        OcompJobStatus::VotingOpen if has_finalized_job => CanonicalJobDispositionV1::VotingOpen,
        OcompJobStatus::Completed if has_finalized_job => CanonicalJobDispositionV1::Completed,
        OcompJobStatus::Expired => CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Expired,
            has_finalized_job,
        },
        OcompJobStatus::Conflicted => CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Conflicted,
            has_finalized_job,
        },
        OcompJobStatus::Canceled => CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Canceled,
            has_finalized_job,
        },
        OcompJobStatus::VotingOpen | OcompJobStatus::Completed => {
            bail!("OCOMP canonical status/finalized payload shape is invalid")
        }
    })
}

/// What startup must do with a persisted watermark, given the canonical chain
/// this node actually has on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointRecoveryV1 {
    /// The checkpoint block is canonical here; resume from it.
    Resume,
    /// The checkpoint outran block persistence; re-scan from the canonical tip.
    RewindTo(OcompExExCheckpointV1),
}

/// Decide how to resume from `checkpoint`.
///
/// The watermark is fsynced independently of reth's block persistence, so an
/// unclean shutdown can leave it naming a block above what was written. That is
/// recoverable: per-job work is reconstructed from canonical notifications, so
/// re-scanning from the canonical tip loses nothing. A checkpoint that is
/// missing at or below the tip, or one whose hash disagrees with canonical, is a
/// genuine inconsistency and still fails closed.
fn classify_checkpoint_recovery(
    checkpoint: OcompExExCheckpointV1,
    canonical_at_checkpoint: Option<B256>,
    canonical_tip: u64,
    canonical_tip_hash: Option<B256>,
) -> eyre::Result<CheckpointRecoveryV1> {
    if let Some(canonical) = canonical_at_checkpoint {
        if canonical == checkpoint.block_hash {
            return Ok(CheckpointRecoveryV1::Resume);
        }
        bail!(
            "OCOMP ExEx checkpoint hash {} conflicts with canonical {} at height {}",
            checkpoint.block_hash,
            canonical,
            checkpoint.block_number
        );
    }
    if checkpoint.block_number <= canonical_tip {
        bail!(
            "OCOMP ExEx checkpoint block {} is unavailable at or below canonical tip {}",
            checkpoint.block_number,
            canonical_tip
        );
    }
    let block_hash = canonical_tip_hash
        .ok_or_else(|| eyre::eyre!("OCOMP ExEx canonical tip {canonical_tip} is unavailable"))?;
    Ok(CheckpointRecoveryV1::RewindTo(OcompExExCheckpointV1 {
        block_number: canonical_tip,
        block_hash,
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalizedHeadActionV1 {
    WaitForRecovery,
    EnterRunning,
    ContinueRunning,
}

fn classify_finalized_head(
    phase: FinalizedHeadPhaseV1,
    scanned_height: u64,
    scanned_hash: B256,
    target_height: u64,
    target_hash: B256,
) -> Result<FinalizedHeadActionV1, &'static str> {
    if target_height < scanned_height {
        return match phase {
            FinalizedHeadPhaseV1::StartupRecovery => Ok(FinalizedHeadActionV1::WaitForRecovery),
            FinalizedHeadPhaseV1::Running => {
                Err("OCOMP finalized head regressed behind the scanned height")
            }
        };
    }
    if target_height == scanned_height && target_hash != scanned_hash {
        return Err("OCOMP finalized head changed hash at the scanned height");
    }
    Ok(match phase {
        FinalizedHeadPhaseV1::StartupRecovery => FinalizedHeadActionV1::EnterRunning,
        FinalizedHeadPhaseV1::Running => FinalizedHeadActionV1::ContinueRunning,
    })
}

fn initial_projection_status(
    checkpoint: Option<OcompExExCheckpointV1>,
    genesis_hash: B256,
) -> ProjectionStatus {
    match checkpoint {
        Some(_) => ProjectionStatus::CatchingUp { checkpoint: None },
        None => ProjectionStatus::Ready {
            checkpoint: ProjectionCheckpoint {
                block_number: 0,
                block_hash: genesis_hash,
            },
        },
    }
}

async fn wait_for_node_teardown() -> ! {
    std::future::pending::<()>().await;
    unreachable!("pending future completed")
}

struct EmbeddedOcompExExV1<P> {
    provider: P,
    policy: EmbeddedNodePolicyV1,
    domain: EmbeddedOcompDomainV1,
    checkpoint: OcompExExCheckpointStoreV1,
    readiness: ProjectionReadinessPublisher,
    exit: tokio::sync::mpsc::UnboundedSender<OcompExExExitV1>,
    requests: BTreeMap<B256, RequestLocatorV1>,
    materialized_requests: BTreeSet<B256>,
    jobs: BTreeMap<B256, RuntimeJobV1>,
    state: EmbeddedOcompJobsV1,
    scanned_height: u64,
    scanned_hash: B256,
    finalized_head_phase: FinalizedHeadPhaseV1,
    compute_tx: mpsc::Sender<EmbeddedComputeOutcomeV1>,
    compute_rx: mpsc::Receiver<EmbeddedComputeOutcomeV1>,
    vote_tx: mpsc::Sender<EmbeddedVoteOutcomeV1>,
    vote_rx: mpsc::Receiver<EmbeddedVoteOutcomeV1>,
    materialization_tx: mpsc::Sender<EmbeddedMaterializationOutcomeV1>,
    materialization_rx: mpsc::Receiver<EmbeddedMaterializationOutcomeV1>,
    materialization_active: Option<MaterializationAttemptKeyV1>,
    payout_tx: mpsc::Sender<EmbeddedPayoutOutcomeV1>,
    payout_rx: mpsc::Receiver<EmbeddedPayoutOutcomeV1>,
    payout_active: bool,
    materialization_attempt_heights: BTreeMap<MaterializationAttemptKeyV1, u64>,
    chain_id: u64,
    genesis_hash: B256,
    fatal: Option<ProjectionFailure>,
}

pub async fn run_ocomp_exex<Node>(
    mut ctx: ExExContext<Node>,
    config: OcompExExConfigV1,
    readiness: ProjectionReadinessPublisher,
    exit: tokio::sync::mpsc::UnboundedSender<OcompExExExitV1>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockIdReader
        + BlockHashReader
        + BlockNumReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    let provider = ctx.provider().clone();
    let domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
        domain_root: config.domain_root,
        registry_generation: 1,
        bundles: config
            .bundles
            .into_iter()
            .map(|bundle| EmbeddedOcompBundleConfigV1 {
                worker_address: bundle.worker_address,
                identity: bundle.identity,
                protocol_bundle: bundle.protocol_bundle,
            })
            .collect(),
        policy: config.policy,
        validator_rpc_url: config.validator_rpc_url,
        limits: poc_schema_limits(),
    })
    .map_err(|error| eyre::eyre!("open Node-owned OCOMP domain: {error}"))?;
    if let Some(detail) = load_persisted_fatal_evidence(domain.fatal_evidence_root())? {
        let failure = ProjectionFailure::new(
            ProjectionFailureClass::Other,
            format!("embedded OCOMP persisted fatal evidence: {detail}"),
        );
        readiness.publish(ProjectionStatus::Fatal {
            checkpoint: None,
            error: failure.clone(),
        });
        let _ = exit.send(OcompExExExitV1 { failure });
        wait_for_node_teardown().await;
    }
    let mut checkpoint = OcompExExCheckpointStoreV1::open(domain.checkpoint_root())
        .wrap_err("open OCOMP ExEx checkpoint")?;
    let loaded_checkpoint = match checkpoint.load() {
        Some(stored) => {
            let canonical_tip = provider
                .best_block_number()
                .wrap_err("read canonical tip for OCOMP ExEx checkpoint recovery")?;
            let recovery = classify_checkpoint_recovery(
                stored,
                provider
                    .block_hash(stored.block_number)
                    .wrap_err("validate OCOMP ExEx checkpoint hash")?,
                canonical_tip,
                provider
                    .block_hash(canonical_tip)
                    .wrap_err("read canonical tip hash for OCOMP ExEx checkpoint recovery")?,
            )?;
            match recovery {
                CheckpointRecoveryV1::Resume => stored,
                CheckpointRecoveryV1::RewindTo(target) => {
                    tracing::warn!(
                        target: "outbe::ocomp::exex",
                        checkpoint_block = stored.block_number,
                        canonical_tip = target.block_number,
                        "OCOMP ExEx checkpoint outran block persistence; re-scanning from the canonical tip"
                    );
                    checkpoint
                        .rewind_to(target)
                        .wrap_err("rewind OCOMP ExEx checkpoint to the canonical tip")?;
                    target
                }
            }
        }
        None => OcompExExCheckpointV1 {
            block_number: 0,
            block_hash: config.genesis_hash,
        },
    };
    // Height 0 carries no notification head: there is nothing scanned to resume
    // from, which is exactly the fresh-start contract.
    let (scanned_height, scanned_hash, finalized_head_phase) =
        if loaded_checkpoint.block_number == 0 {
            (0, config.genesis_hash, FinalizedHeadPhaseV1::Running)
        } else {
            ctx.set_notifications_with_head(ExExHead::new(
                (loaded_checkpoint.block_number, loaded_checkpoint.block_hash).into(),
            ));
            (
                loaded_checkpoint.block_number,
                loaded_checkpoint.block_hash,
                FinalizedHeadPhaseV1::StartupRecovery,
            )
        };
    let loaded_checkpoint = (loaded_checkpoint.block_number != 0).then_some(loaded_checkpoint);
    readiness.publish(initial_projection_status(
        loaded_checkpoint,
        config.genesis_hash,
    ));
    let mode = match config.policy {
        EmbeddedNodePolicyV1::Validator => EmbeddedOcompModeV1::Validator,
        EmbeddedNodePolicyV1::FullNode => EmbeddedOcompModeV1::FullNode,
    };
    let (compute_tx, compute_rx) = mpsc::channel();
    let (vote_tx, vote_rx) = mpsc::channel();
    let (materialization_tx, materialization_rx) = mpsc::channel();
    let (payout_tx, payout_rx) = mpsc::channel();
    let mut runtime = EmbeddedOcompExExV1 {
        provider,
        policy: config.policy,
        domain,
        checkpoint,
        readiness,
        exit,
        requests: BTreeMap::new(),
        materialized_requests: BTreeSet::new(),
        jobs: BTreeMap::new(),
        state: EmbeddedOcompJobsV1::new(mode),
        scanned_height,
        scanned_hash,
        finalized_head_phase,
        compute_tx,
        compute_rx,
        vote_tx,
        vote_rx,
        materialization_tx,
        materialization_rx,
        materialization_active: None,
        payout_tx,
        payout_rx,
        payout_active: false,
        materialization_attempt_heights: BTreeMap::new(),
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        fatal: None,
    };

    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut notifications_open = true;
    loop {
        tokio::select! {
            notification = ctx.notifications.next(), if notifications_open => {
                match classify_exex_notification(notification) {
                    ExExNotificationDispositionV1::Continue => {}
                    ExExNotificationDispositionV1::CloseAfterError(error) => {
                        warn!(%error, "failed to drain OCOMP ExEx notification; closing auxiliary stream");
                        notifications_open = false;
                    }
                    ExExNotificationDispositionV1::Closed => {
                        notifications_open = false;
                        warn!("OCOMP ExEx notification stream closed");
                    }
                }
            }
            _ = poll.tick() => {
                if let Err(error) = runtime.reconcile() {
                    runtime.latch_fatal(B256::ZERO, error.to_string())?;
                    wait_for_node_teardown().await;
                }
                while let Ok(outcome) = runtime.compute_rx.try_recv() {
                    if let Err(error) = runtime.handle_compute(outcome) {
                        runtime.latch_fatal(B256::ZERO, error.to_string())?;
                        wait_for_node_teardown().await;
                    }
                    if runtime.fatal.is_some() {
                        wait_for_node_teardown().await;
                    }
                }
                while let Ok(outcome) = runtime.vote_rx.try_recv() {
                    if let Err(error) = runtime.handle_vote(outcome) {
                        runtime.latch_fatal(B256::ZERO, error.to_string())?;
                        wait_for_node_teardown().await;
                    }
                    if runtime.fatal.is_some() {
                        wait_for_node_teardown().await;
                    }
                }
                while let Ok(outcome) = runtime.materialization_rx.try_recv() {
                    runtime.handle_materialization(outcome);
                }
                while let Ok(outcome) = runtime.payout_rx.try_recv() {
                    runtime.handle_payout(outcome);
                }
                if let Some(checkpoint) = runtime.advance_checkpoint()? {
                    if ctx.events.send(ExExEvent::FinishedHeight(
                        (checkpoint.block_number, checkpoint.block_hash).into()
                    )).is_err() {
                        warn!("failed to publish OCOMP ExEx finished height");
                    }
                }
            }
        }
    }
}

impl<P> EmbeddedOcompExExV1<P>
where
    P: BlockIdReader
        + BlockHashReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn reconcile(&mut self) -> eyre::Result<()> {
        let Some(target) = self
            .provider
            .finalized_block_num_hash()
            .wrap_err("sample OCOMP finalized head")?
        else {
            return Ok(());
        };
        match classify_finalized_head(
            self.finalized_head_phase,
            self.scanned_height,
            self.scanned_hash,
            target.number,
            target.hash,
        )
        .map_err(eyre::Report::msg)?
        {
            FinalizedHeadActionV1::WaitForRecovery => return Ok(()),
            FinalizedHeadActionV1::EnterRunning => {
                self.finalized_head_phase = FinalizedHeadPhaseV1::Running;
                info!(
                    checkpoint_height = self.scanned_height,
                    target_height = target.number,
                    "embedded OCOMP finalized checkpoint recovery completed"
                );
            }
            FinalizedHeadActionV1::ContinueRunning => {}
        }
        if target.number == self.scanned_height {
            self.refresh_jobs(target.number, target.hash)?;
            self.reconcile_materialization(target.number, target.hash)?;
            self.drive_payout(target.hash);
            self.publish_readiness(target.number, target.hash)?;
            return Ok(());
        }
        for number in self.scanned_height.saturating_add(1)..=target.number {
            let hash = self
                .provider
                .block_hash(number)
                .wrap_err_with(|| format!("load OCOMP canonical hash at {number}"))?
                .ok_or_else(|| eyre::eyre!("OCOMP canonical block {number} is unavailable"))?;
            self.scan_requests(number, hash)?;
            self.refresh_jobs(number, hash)?;
            self.reconcile_materialization(number, hash)?;
            self.drive_payout(hash);
            self.scanned_height = number;
            self.scanned_hash = hash;
            self.publish_readiness(number, hash)?;
        }
        if self.scanned_hash != target.hash {
            bail!("OCOMP scanned finalized target hash mismatch");
        }
        Ok(())
    }

    fn scan_requests(&mut self, number: u64, hash: B256) -> eyre::Result<()> {
        let block = self
            .provider
            .block_by_hash(hash)
            .wrap_err("load OCOMP request block")?
            .ok_or_else(|| eyre::eyre!("OCOMP request block is unavailable"))?;
        if block.header().number() != number || block.header().hash_slow() != hash {
            bail!("OCOMP provider returned a conflicting canonical block");
        }
        let receipts = self
            .provider
            .receipts_by_block(hash.into())
            .wrap_err("load OCOMP request receipts")?
            .ok_or_else(|| eyre::eyre!("OCOMP request receipts are unavailable"))?;
        if block.body().transactions().len() != receipts.len() {
            bail!("OCOMP request transaction/receipt count mismatch");
        }
        for receipt in receipts {
            if !receipt.status() {
                continue;
            }
            for log in receipt.logs() {
                if log.address != METADOSIS_ADDRESS
                    || log.data.topics().first()
                        != Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
                {
                    continue;
                }
                let event = IMetadosis::OffchainJobRequested::decode_log(log)
                    .wrap_err("decode OCOMP request locator")?;
                let locator = RequestLocatorV1 {
                    intent_id: event.data.intentId,
                    wwd: event.data.wwd,
                    pending_nonce: event.data.pendingNonce,
                    attempt: event.data.attempt,
                    activation_preconditions_hash: event.data.activationPreconditionsHash,
                    block_number: number,
                    block_hash: hash,
                    state_root: block.header().state_root(),
                };
                self.validate_request(&locator, hash)?;
                match self.requests.get(&locator.intent_id) {
                    Some(existing) if same_locator(*existing, locator) => {}
                    Some(_) => {
                        bail!("conflicting OCOMP request locator replay");
                    }
                    None => {
                        self.requests.insert(locator.intent_id, locator);
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_request(&self, locator: &RequestLocatorV1, state_hash: B256) -> eyre::Result<()> {
        let limits = poc_schema_limits();
        let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
            &self.provider,
            state_hash,
            locator.intent_id,
            &limits,
        )
        .wrap_err("read exact OCOMP request record")?;
        let activation_hash = record
            .intent
            .activation_preconditions
            .activation_preconditions_hash(&limits)
            .wrap_err("hash OCOMP request activation preconditions")?;
        if record.intent_height != locator.block_number
            || record.intent.wwd != locator.wwd
            || record.intent.pending_nonce != locator.pending_nonce
            || record.intent.attempt != locator.attempt
            || activation_hash != locator.activation_preconditions_hash
        {
            bail!("OCOMP request locator disagrees with exact typed state");
        }
        Ok(())
    }

    fn refresh_jobs(&mut self, height: u64, hash: B256) -> eyre::Result<()> {
        let limits = poc_schema_limits();
        let locators = self.requests.values().copied().collect::<Vec<_>>();
        for locator in locators {
            let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
                &self.provider,
                hash,
                locator.intent_id,
                &limits,
            )
            .wrap_err("read current exact OCOMP job record")?;
            let disposition = classify_canonical_job(record.status, record.finalized.is_some())?;
            match disposition {
                CanonicalJobDispositionV1::AwaitingFinality => continue,
                CanonicalJobDispositionV1::Closed {
                    has_finalized_job: false,
                    ..
                } => {
                    self.materialized_requests.insert(locator.intent_id);
                    continue;
                }
                CanonicalJobDispositionV1::FinalizedAwaitingOpen
                | CanonicalJobDispositionV1::VotingOpen
                | CanonicalJobDispositionV1::Completed
                | CanonicalJobDispositionV1::Closed {
                    has_finalized_job: true,
                    ..
                } => {}
            }
            let finalized = record
                .finalized
                .as_ref()
                .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
            if finalized.finalized_request_block_hash != locator.block_hash
                || finalized.finalized_request_state_root != locator.state_root
            {
                bail!("OCOMP finalized job disagrees with its request block identity");
            }
            let job_id = record
                .intent
                .job_id(locator.block_hash, locator.state_root, &limits)
                .wrap_err("derive finalized OCOMP JobId")?;
            if job_id != finalized.job_id {
                bail!("OCOMP finalized job carries a conflicting JobId");
            }
            let discovered = !self.jobs.contains_key(&job_id);
            if discovered {
                let discovery = discovery_record(locator, &record, job_id, &limits)?;
                let generation = self
                    .state
                    .observe_job(job_id, finalized.deadline_height)
                    .wrap_err("observe embedded OCOMP job")?;
                let cancelled = Arc::new(AtomicBool::new(false));
                self.jobs.insert(
                    job_id,
                    RuntimeJobV1 {
                        record: discovery,
                        generation,
                        cancelled,
                        compute_started: false,
                        vote_eligibility: if self.policy == EmbeddedNodePolicyV1::Validator {
                            LocalVoteEligibilityV1::Pending
                        } else {
                            LocalVoteEligibilityV1::NotMember
                        },
                        vote_started: false,
                        canonical_result: None,
                    },
                );
            }
            self.materialized_requests.insert(locator.intent_id);

            match disposition {
                CanonicalJobDispositionV1::FinalizedAwaitingOpen => {}
                CanonicalJobDispositionV1::VotingOpen => {
                    let eligibility_became_available =
                        self.refresh_vote_eligibility(locator, &record, job_id)?;
                    let compute_started = self
                        .jobs
                        .get(&job_id)
                        .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
                        .compute_started;
                    if local_result_restore_policy(
                        disposition,
                        self.policy,
                        discovered,
                        eligibility_became_available,
                        compute_started,
                    ) == LocalResultRestorePolicyV1::BeforeCompute
                    {
                        self.restore_local_result(job_id)?;
                    }
                    self.ensure_compute_started(job_id)?;
                }
                CanonicalJobDispositionV1::Completed => {
                    self.observe_completed(job_id, &record)?;
                    if self.policy == EmbeddedNodePolicyV1::FullNode {
                        let compute_started = self
                            .jobs
                            .get(&job_id)
                            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
                            .compute_started;
                        if local_result_restore_policy(
                            disposition,
                            self.policy,
                            discovered,
                            false,
                            compute_started,
                        ) == LocalResultRestorePolicyV1::AfterCanonicalCompleted
                        {
                            self.restore_local_result(job_id)?;
                        }
                        self.ensure_compute_started(job_id)?;
                    }
                }
                CanonicalJobDispositionV1::Closed { reason, .. } => {
                    self.observe_terminal(job_id, reason)?;
                }
                CanonicalJobDispositionV1::AwaitingFinality => {
                    unreachable!("awaiting-finality records returned before job discovery")
                }
            }
            if height >= finalized.deadline_height
                && matches!(
                    self.state.state(job_id),
                    Some(
                        EmbeddedJobStateV1::Computing
                            | EmbeddedJobStateV1::LocalReady
                            | EmbeddedJobStateV1::WaitAtDeadline
                    )
                )
            {
                self.state
                    .reduce(job_id, EmbeddedJobEventV1::Deadline)
                    .wrap_err("observe OCOMP deadline")?;
            }
        }
        Ok(())
    }

    fn ensure_compute_started(&mut self, job_id: B256) -> eyre::Result<()> {
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?;
        if job.compute_started {
            return Ok(());
        }
        self.domain.spawn_compute(
            job.record.clone(),
            job.generation,
            Arc::clone(&job.cancelled),
            self.compute_tx.clone(),
        )?;
        job.compute_started = true;
        info!(%job_id, "embedded OCOMP computation started");
        Ok(())
    }

    fn restore_local_result(&mut self, job_id: B256) -> eyre::Result<()> {
        let Some(loaded) = self.domain.load_local_result(job_id)? else {
            return Ok(());
        };
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("restored OCOMP job is unknown"))?
            .compute_started = true;
        let generation = self
            .jobs
            .get(&job_id)
            .ok_or_else(|| eyre::eyre!("restored OCOMP job is unknown"))?
            .generation;
        self.accept_local_result(
            job_id,
            generation,
            loaded.committed.result_digest,
            loaded.canonical_result,
        )?;
        info!(
            %job_id,
            result_digest = %loaded.committed.result_digest,
            "restored durable embedded OCOMP local result"
        );
        Ok(())
    }

    fn observe_completed(&mut self, job_id: B256, record: &OcompJobRecordV1) -> eyre::Result<()> {
        if self
            .jobs
            .get(&job_id)
            .and_then(|job| job.canonical_result.as_ref())
            .is_some()
        {
            return Ok(());
        }
        let terminal = record
            .terminal
            .as_ref()
            .ok_or_else(|| eyre::eyre!("completed OCOMP job lacks terminal record"))?;
        let binding = terminal
            .completed_binding
            .as_ref()
            .ok_or_else(|| eyre::eyre!("completed OCOMP job lacks completed binding"))?;
        let vote = canonical_vote_at(
            &self.provider,
            binding.quorum_height,
            record,
            binding.result_digest,
        )?;
        let canonical = vote.result;
        let digest = canonical.result_digest(&poc_schema_limits())?;
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .canonical_result = Some(canonical.clone());
        let action = self
            .state
            .reduce(
                job_id,
                EmbeddedJobEventV1::CanonicalCompleted {
                    result_digest: digest,
                },
            )
            .wrap_err("record canonical OCOMP result")?
            .action;
        if matches!(action, EmbeddedJobActionV1::ReleaseProgress { .. }) {
            self.verify_full_node_exact(job_id, &canonical)?;
        } else if let EmbeddedJobActionV1::FatalMismatch {
            local_result_digest,
            canonical_result_digest,
            ..
        } = action
        {
            self.persist_and_publish_mismatch(
                job_id,
                local_result_digest,
                canonical_result_digest,
            )?;
        }
        Ok(())
    }

    fn observe_terminal(
        &mut self,
        job_id: B256,
        reason: EmbeddedTerminalReasonV1,
    ) -> eyre::Result<()> {
        self.state
            .reduce(job_id, EmbeddedJobEventV1::CanonicalClosed { reason })
            .wrap_err("record finalized OCOMP terminal state")?;
        if let Some(job) = self.jobs.get(&job_id) {
            job.cancelled.store(true, Ordering::Release);
        }
        info!(%job_id, ?reason, "embedded OCOMP job reached canonical terminal state");
        Ok(())
    }

    fn handle_compute(&mut self, outcome: EmbeddedComputeOutcomeV1) -> eyre::Result<()> {
        let (generation, completed) = match outcome {
            EmbeddedComputeOutcomeV1::Completed {
                generation,
                completed,
            } => (generation, completed),
            EmbeddedComputeOutcomeV1::Unrecoverable {
                job_id,
                generation,
                detail,
            } => {
                let Some(projection) = self.async_outcome_projection(job_id)? else {
                    return Ok(());
                };
                if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
                    info!(%job_id, %detail, "ignored checkpoint-pruned OCOMP computation failure");
                    return Ok(());
                }
                let action = self
                    .state
                    .reduce(job_id, EmbeddedJobEventV1::LocalFailed { generation })
                    .wrap_err("reduce embedded OCOMP local failure")?
                    .action;
                match action {
                    EmbeddedJobActionV1::FatalLocalFailure => {
                        persist_local_failure_evidence(
                            self.domain.fatal_evidence_root(),
                            job_id,
                            &detail,
                        )?;
                        self.latch_fatal(job_id, detail)?;
                    }
                    EmbeddedJobActionV1::Abstain => {
                        error!(%job_id, %detail, "Validator OCOMP computation failed; abstaining from vote");
                    }
                    EmbeddedJobActionV1::ProtocolOwned => {
                        info!(%job_id, %detail, "ignored late embedded OCOMP computation failure");
                    }
                    _ => bail!("unexpected embedded OCOMP local-failure action"),
                }
                return Ok(());
            }
        };
        let job_id = completed.job_id;
        let Some(projection) = self.async_outcome_projection(job_id)? else {
            return Ok(());
        };
        let committed = match self.domain.commit_local_result(&completed) {
            Ok(committed) => committed,
            Err(error) if projection == AsyncOutcomeProjectionV1::CheckpointPruned => {
                warn!(%job_id, %error, "failed to persist checkpoint-pruned OCOMP local result");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
            info!(
                %job_id,
                result_digest = %committed.result_digest,
                "persisted checkpoint-pruned OCOMP local result without reopening protocol state"
            );
            return Ok(());
        }
        self.accept_local_result(
            job_id,
            generation,
            committed.result_digest,
            completed.canonical_result,
        )
    }

    fn accept_local_result(
        &mut self,
        job_id: B256,
        generation: EmbeddedJobGenerationV1,
        result_digest: B256,
        canonical_result: Vec<u8>,
    ) -> eyre::Result<()> {
        let action = self
            .state
            .reduce(
                job_id,
                EmbeddedJobEventV1::LocalCompleted {
                    generation,
                    result_digest,
                },
            )
            .wrap_err("record embedded OCOMP local result")?
            .action;
        match action {
            EmbeddedJobActionV1::SubmitVote { .. } => {
                let job = self
                    .jobs
                    .get_mut(&job_id)
                    .ok_or_else(|| eyre::eyre!("computed OCOMP job is unknown"))?;
                match job.vote_eligibility {
                    LocalVoteEligibilityV1::Pending => {
                        info!(%job_id, "pinned OCOMP vote membership is temporarily unavailable; retaining local result");
                    }
                    LocalVoteEligibilityV1::NotMember => {
                        if !job.vote_started {
                            info!(%job_id, "embedded OCOMP Validator is not in the pinned snapshot; abstaining");
                            job.vote_started = true;
                        }
                    }
                    LocalVoteEligibilityV1::Eligible if !job.vote_started => {
                        self.domain.spawn_validator_vote(
                            job.record.clone(),
                            job.generation,
                            result_digest,
                            canonical_result,
                            Arc::clone(&job.cancelled),
                            self.vote_tx.clone(),
                        )?;
                        job.vote_started = true;
                    }
                    LocalVoteEligibilityV1::Eligible => {}
                }
            }
            EmbeddedJobActionV1::ReleaseProgress { .. } => {
                let canonical = self
                    .jobs
                    .get(&job_id)
                    .and_then(|job| job.canonical_result.clone())
                    .ok_or_else(|| eyre::eyre!("canonical OCOMP result disappeared"))?;
                self.verify_full_node_exact(job_id, &canonical)?;
            }
            EmbeddedJobActionV1::FatalMismatch {
                local_result_digest,
                canonical_result_digest,
                ..
            } => self.persist_and_publish_mismatch(
                job_id,
                local_result_digest,
                canonical_result_digest,
            )?,
            EmbeddedJobActionV1::AwaitCanonical { .. } => {}
            // A quorum can form before the local computation finishes; the job
            // is then already canonical-settled and no local action remains.
            EmbeddedJobActionV1::ProtocolOwned => {
                info!(
                    %job_id,
                    %result_digest,
                    "embedded OCOMP local result arrived after canonical settlement; protocol owns the job"
                );
            }
            EmbeddedJobActionV1::AwaitLocalResult { .. }
            | EmbeddedJobActionV1::HoldProgress { .. }
            | EmbeddedJobActionV1::CloseTerminal { .. }
            | EmbeddedJobActionV1::Abstain
            | EmbeddedJobActionV1::FatalLocalFailure
            | EmbeddedJobActionV1::VoteFinalized { .. }
            | EmbeddedJobActionV1::FatalVoteFailure => {
                bail!("unexpected embedded OCOMP local-result action");
            }
        }
        Ok(())
    }

    fn refresh_vote_eligibility(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
    ) -> eyre::Result<bool> {
        if self.policy != EmbeddedNodePolicyV1::Validator {
            return Ok(false);
        }
        let current = self
            .jobs
            .get(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .vote_eligibility;
        if current != LocalVoteEligibilityV1::Pending {
            return Ok(false);
        }
        let Some(ocomp_key_hash) = self.domain.validator_ocomp_key_hash() else {
            bail!("Validator OCOMP key hash is unavailable");
        };
        let observed = outbe_node::ocomp::retention::ocomp_snapshot_contains_key_at(
            &self.provider,
            locator.block_hash,
            &record.intent,
            ocomp_key_hash,
        );
        if let outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Unavailable { detail } =
            &observed
        {
            warn!(%job_id, %detail, "pinned OCOMP vote membership is temporarily unavailable");
        }
        let resolved = advance_vote_eligibility(current, observed)?;
        if resolved == LocalVoteEligibilityV1::Pending {
            return Ok(false);
        }
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .vote_eligibility = resolved;
        Ok(resolved == LocalVoteEligibilityV1::Eligible)
    }

    fn handle_vote(&mut self, outcome: EmbeddedVoteOutcomeV1) -> eyre::Result<()> {
        let (job_id, event, detail) = match outcome {
            EmbeddedVoteOutcomeV1::Finalized {
                job_id,
                generation,
                success,
            } => (
                job_id,
                EmbeddedJobEventV1::VoteFinalized {
                    generation,
                    success,
                },
                None,
            ),
            EmbeddedVoteOutcomeV1::Unrecoverable {
                job_id,
                generation,
                detail,
            } => (
                job_id,
                EmbeddedJobEventV1::VoteFailed { generation },
                Some(detail),
            ),
        };
        let Some(projection) = self.async_outcome_projection(job_id)? else {
            return Ok(());
        };
        if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
            info!(%job_id, "ignored checkpoint-pruned embedded OCOMP vote outcome");
            return Ok(());
        }
        match self
            .state
            .reduce(job_id, event)
            .wrap_err("reduce embedded OCOMP vote outcome")?
            .action
        {
            EmbeddedJobActionV1::VoteFinalized { success } => {
                info!(%job_id, success, "embedded OCOMP vote finalized");
            }
            EmbeddedJobActionV1::FatalVoteFailure => {
                self.latch_fatal(
                    job_id,
                    detail.unwrap_or_else(|| "embedded OCOMP vote failed".to_owned()),
                )?;
            }
            EmbeddedJobActionV1::ProtocolOwned => {
                info!(%job_id, "ignored late embedded OCOMP vote outcome");
            }
            _ => bail!("unexpected embedded OCOMP vote action"),
        }
        Ok(())
    }

    fn async_outcome_projection(
        &mut self,
        job_id: B256,
    ) -> eyre::Result<Option<AsyncOutcomeProjectionV1>> {
        let classified = classify_async_outcome_projection(
            self.jobs.get(&job_id).map(|job| job.generation),
            self.state.generation(job_id),
        );
        match classified {
            Ok(projection) => Ok(Some(projection)),
            Err(error) => {
                self.latch_fatal(job_id, error.to_string())?;
                Ok(None)
            }
        }
    }

    fn reconcile_materialization(&mut self, height: u64, hash: B256) -> eyre::Result<()> {
        let block = self
            .provider
            .block_by_hash(hash)
            .wrap_err("load NOD materialization trigger block")?
            .ok_or_else(|| eyre::eyre!("NOD materialization trigger block is unavailable"))?;
        let Some(proposer) =
            finalized_materialization_proposer(height, block.body().transactions())?
        else {
            return Ok(());
        };
        let state = self
            .provider
            .state_by_block_hash(hash)
            .wrap_err("open NOD materialization finalized state")?;
        let reader = OcompExExStateReaderV1 {
            state: state.as_ref(),
        };
        let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
            reader,
            self.chain_id,
            self.genesis_hash,
        );
        let storage = StorageHandle::new(&mut readonly);
        let head = outbe_nod::NodContract::new(storage.clone())
            .ocomp_materialization_head()
            .wrap_err("read finalized NOD materialization head")?;
        let current_attempt = head.as_ref().map(|head| MaterializationAttemptKeyV1 {
            queue_sequence: head.queue_sequence,
            first_nod_ordinal: head.next_nod_ordinal,
        });
        bound_materialization_attempts(&mut self.materialization_attempt_heights, current_attempt);
        let profile = outbe_chain_constants::NodMaterializationProfileV1 {
            batch_subtree_height:
                outbe_chain_constants::get_nod_materialization_batch_subtree_height(),
            retry_interval_blocks:
                outbe_chain_constants::get_nod_materialization_retry_interval_blocks(),
            max_attempts_per_block:
                outbe_chain_constants::get_nod_materialization_max_attempts_per_block(),
        };
        let represented_validator = match self.domain.validator_sender_address() {
            Some(sender) => outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .resolve_validator_for_role(
                    sender,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                )
                .wrap_err("resolve finalized OCOMP materializer")?,
            None => None,
        };
        let progress_observed = head
            .as_ref()
            .is_some_and(|head| head.last_progress_height == height);
        if !should_wake_nod_materializer(
            self.policy,
            represented_validator,
            proposer,
            height,
            head.as_ref(),
            progress_observed,
            profile.retry_interval_blocks,
        ) {
            return Ok(());
        }
        let head = head.ok_or_else(|| eyre::eyre!("materialization wake lost FIFO head"))?;
        let key = MaterializationAttemptKeyV1 {
            queue_sequence: head.queue_sequence,
            first_nod_ordinal: head.next_nod_ordinal,
        };
        if self.materialization_active.is_some()
            || self.materialization_attempt_heights.get(&key) == Some(&height)
        {
            return Ok(());
        }
        // The bundle the generation was certified under is chain state, not runtime
        // state: a node that has forgotten every terminal job still materializes.
        let protocol_bundle_hash = outbe_nod::NodContract::new(storage)
            .ocomp_certified_generation(outbe_common::WorldwideDay::from(head.worldwide_day))?
            .ok_or_else(|| eyre::eyre!("materialization head has no certified generation"))?
            .protocol_bundle_hash;
        self.domain.spawn_validator_materialization(
            protocol_bundle_hash,
            head,
            profile.batch_subtree_height,
            self.materialization_tx.clone(),
        )?;
        self.materialization_active = Some(key);
        self.materialization_attempt_heights.insert(key, height);
        info!(
            queue_sequence = key.queue_sequence,
            first_nod_ordinal = key.first_nod_ordinal,
            "finalized proposer woke NOD materialization"
        );
        Ok(())
    }

    /// Ticks the payout sender owned by the embedded Supervisor.
    /// One tick at a time: the submitter journals its own progress, so a later
    /// block simply resumes where this one stopped.
    fn drive_payout(&mut self, head: B256) {
        if self.payout_active {
            return;
        }
        // The chain's own clock, not the host's: a localnet can run a shifted
        // genesis, and a day the host has not reached yet still owes its payout.
        let Ok(Some(block)) = self.provider.block_by_hash(head) else {
            return;
        };
        let mut day =
            outbe_primitives::time::worldwide_day_from_timestamp(block.header().timestamp());
        let mut days = Vec::with_capacity(PAYOUT_LOOKBACK_DAYS as usize + 1);
        for _ in 0..=PAYOUT_LOOKBACK_DAYS {
            days.push(day);
            day = outbe_primitives::time::previous_date_key(day);
        }
        days.reverse();
        match self.domain.spawn_validator_payout(
            days,
            Arc::new(AtomicBool::new(false)),
            self.payout_tx.clone(),
        ) {
            Ok(()) => self.payout_active = true,
            Err(EmbeddedOcompRuntimeErrorV1::FullNodeVoteAuthority) => {}
            Err(error) => warn!(?error, "OCOMP payout tick could not start"),
        }
    }

    fn handle_payout(&mut self, outcome: EmbeddedPayoutOutcomeV1) {
        self.payout_active = false;
        match outcome {
            EmbeddedPayoutOutcomeV1::Ticked(PayoutTickOutcomeV1::Idle) => {}
            EmbeddedPayoutOutcomeV1::Ticked(outcome) => {
                info!(?outcome, "OCOMP payout tick advanced");
            }
            EmbeddedPayoutOutcomeV1::Failed(detail) => {
                warn!(detail, "OCOMP payout tick failed");
            }
        }
    }

    fn handle_materialization(&mut self, outcome: EmbeddedMaterializationOutcomeV1) {
        let (key, success, detail) = match outcome {
            EmbeddedMaterializationOutcomeV1::Finalized {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                success,
            } => {
                info!(%job_id, queue_sequence, first_nod_ordinal, success, "NOD materialization transaction finalized");
                (
                    MaterializationAttemptKeyV1 {
                        queue_sequence,
                        first_nod_ordinal,
                    },
                    Some(success),
                    None,
                )
            }
            EmbeddedMaterializationOutcomeV1::Unavailable {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                detail,
            } => {
                warn!(%job_id, queue_sequence, first_nod_ordinal, %detail, "NOD materialization attempt unavailable");
                (
                    MaterializationAttemptKeyV1 {
                        queue_sequence,
                        first_nod_ordinal,
                    },
                    None,
                    Some(detail),
                )
            }
        };
        if self.materialization_active == Some(key) {
            self.materialization_active = None;
        }
        let _ = (success, detail);
    }

    fn verify_full_node_exact(&self, job_id: B256, canonical: &LysisResultV1) -> eyre::Result<()> {
        if self.policy == EmbeddedNodePolicyV1::FullNode {
            self.domain
                .verify_exact_canonical_result(job_id, canonical)?;
        }
        Ok(())
    }

    fn publish_readiness(&self, finalized_height: u64, finalized_hash: B256) -> eyre::Result<()> {
        if self.policy != EmbeddedNodePolicyV1::FullNode || self.fatal.is_some() {
            return Ok(());
        }
        let allowed = self.state.progress_limit(finalized_height);
        let hash = if allowed == finalized_height {
            finalized_hash
        } else {
            self.provider
                .block_hash(allowed)
                .wrap_err("load OCOMP readiness checkpoint hash")?
                .ok_or_else(|| eyre::eyre!("OCOMP readiness checkpoint block is unavailable"))?
        };
        let checkpoint = ProjectionCheckpoint {
            block_number: allowed,
            block_hash: hash,
        };
        self.readiness.publish(if allowed == finalized_height {
            ProjectionStatus::Ready { checkpoint }
        } else {
            ProjectionStatus::CatchingUp {
                checkpoint: Some(checkpoint),
            }
        });
        Ok(())
    }

    fn advance_checkpoint(&mut self) -> eyre::Result<Option<OcompExExCheckpointV1>> {
        if self.fatal.is_some()
            || self.state.live_job_count() != 0
            || self.scanned_height == 0
            || !all_requests_materialized(
                self.requests.keys().copied(),
                &self.materialized_requests,
            )
        {
            return Ok(None);
        }
        let next = OcompExExCheckpointV1 {
            block_number: self.scanned_height,
            block_hash: self.scanned_hash,
        };
        if self.checkpoint.load() == Some(next) {
            self.prune_checkpointed_projection();
            return Ok(None);
        }
        self.checkpoint.persist(next)?;
        self.prune_checkpointed_projection();
        Ok(Some(next))
    }

    fn prune_checkpointed_projection(&mut self) {
        debug_assert_eq!(self.state.live_job_count(), 0);
        self.requests.clear();
        self.materialized_requests.clear();
        self.jobs.clear();
        self.state.prune_terminal();
    }

    fn persist_and_publish_mismatch(
        &mut self,
        job_id: B256,
        local: B256,
        canonical: B256,
    ) -> eyre::Result<()> {
        persist_fatal_evidence(self.domain.fatal_evidence_root(), job_id, local, canonical)?;
        self.latch_fatal(
            job_id,
            format!("local result {local} differs from canonical result {canonical}"),
        )
    }

    fn latch_fatal(&mut self, job_id: B256, detail: String) -> eyre::Result<()> {
        if self.fatal.is_some() {
            return Ok(());
        }
        persist_generic_fatal_evidence(self.domain.fatal_evidence_root(), job_id, &detail)?;
        error!(%job_id, %detail, "embedded OCOMP requested local node shutdown");
        let failure = ProjectionFailure::new(
            ProjectionFailureClass::Other,
            format!("embedded OCOMP job {job_id}: {detail}"),
        );
        let allowed_height = self.state.progress_limit(self.scanned_height);
        let checkpoint = if allowed_height == self.scanned_height {
            Some(ProjectionCheckpoint {
                block_number: allowed_height,
                block_hash: self.scanned_hash,
            })
        } else {
            self.provider
                .block_hash(allowed_height)
                .ok()
                .flatten()
                .map(|block_hash| ProjectionCheckpoint {
                    block_number: allowed_height,
                    block_hash,
                })
        };
        self.fatal = Some(failure.clone());
        self.readiness.publish(ProjectionStatus::Fatal {
            checkpoint,
            error: failure.clone(),
        });
        let _ = self.exit.send(OcompExExExitV1 { failure });
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ExExNotificationDispositionV1<E> {
    Continue,
    CloseAfterError(E),
    Closed,
}

fn classify_exex_notification<T, E>(
    notification: Option<Result<T, E>>,
) -> ExExNotificationDispositionV1<E> {
    match notification {
        Some(Ok(_)) => ExExNotificationDispositionV1::Continue,
        Some(Err(error)) => ExExNotificationDispositionV1::CloseAfterError(error),
        None => ExExNotificationDispositionV1::Closed,
    }
}

fn all_requests_materialized(
    request_ids: impl IntoIterator<Item = B256>,
    materialized_requests: &BTreeSet<B256>,
) -> bool {
    request_ids
        .into_iter()
        .all(|intent_id| materialized_requests.contains(&intent_id))
}

fn discovery_record(
    locator: RequestLocatorV1,
    record: &OcompJobRecordV1,
    job_id: B256,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> eyre::Result<DiscoveryRecord> {
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("OCOMP job is not finalized"))?;
    let spec = FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: locator.block_number,
            job_id,
            intent_id: locator.intent_id,
            finalized_block_hash: locator.block_hash,
            finalized_state_root: locator.state_root,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
        },
        canonical_job_intent: BoundedBytes(record.intent.encode_canonical(limits)?),
    };
    spec.encode_body(limits)?;
    Ok(DiscoveryRecord {
        generation: locator.block_number.max(1),
        cursor: locator.block_number,
        spec,
    })
}

fn canonical_vote_at<P>(
    provider: &P,
    quorum_height: u64,
    record: &OcompJobRecordV1,
    expected_digest: B256,
) -> eyre::Result<ResultVoteV1>
where
    P: BlockHashReader + BlockReader + ReceiptProvider<Receipt = OutbeReceipt>,
{
    let hash = provider
        .block_hash(quorum_height)
        .wrap_err("load q-forming OCOMP block hash")?
        .ok_or_else(|| eyre::eyre!("q-forming OCOMP block is unavailable"))?;
    let block = provider
        .block_by_hash(hash)
        .wrap_err("load q-forming OCOMP block")?
        .ok_or_else(|| eyre::eyre!("q-forming OCOMP block body is unavailable"))?;
    let receipts = provider
        .receipts_by_block(hash.into())
        .wrap_err("load q-forming OCOMP receipts")?
        .ok_or_else(|| eyre::eyre!("q-forming OCOMP receipts are unavailable"))?;
    if block.body().transactions().len() != receipts.len() {
        bail!("q-forming OCOMP transaction/receipt count mismatch");
    }
    let limits = poc_schema_limits();
    let intent_id = record.intent.intent_id(&limits)?;
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("completed OCOMP job is not finalized"))?;
    let mut matched = None;
    for (transaction, receipt) in block.body().transactions().iter().zip(receipts) {
        if !receipt.status() {
            continue;
        }
        let activates = receipt.logs().iter().any(|log| {
            if log.address != METADOSIS_ADDRESS
                || log.data.topics().first() != Some(&IMetadosis::LysisActivated::SIGNATURE_HASH)
            {
                return false;
            }
            IMetadosis::LysisActivated::decode_log(log).is_ok_and(|event| {
                event.data.intentId == intent_id
                    && event.data.jobId == finalized.job_id
                    && event.data.resultDigest == expected_digest
            })
        });
        if !activates || transaction.to() != Some(METADOSIS_ADDRESS) {
            continue;
        }
        let vote = decode_submit_lysis_result(transaction.input().as_ref(), &limits)
            .wrap_err("decode q-forming ResultVoteV1")?;
        let digest = vote.result_digest(&limits)?;
        vote.result.validate_finalized_intent(&record.intent)?;
        if vote.job_id != finalized.job_id
            || vote.protocol_bundle_hash != record.intent.protocol_bundle_hash
            || vote.attempt != record.intent.attempt
            || vote.result_validator_set_epoch != record.intent.result_validator_set_epoch
            || vote.result_committee_set_hash != record.intent.result_committee_set_hash
            || vote.result_ocomp_binding_hash != record.intent.result_ocomp_binding_hash
            || digest != expected_digest
        {
            bail!("q-forming ResultVoteV1 disagrees with finalized OCOMP authority");
        }
        if matched.replace(vote).is_some() {
            bail!("more than one q-forming OCOMP vote matched one completed job");
        }
    }
    matched.ok_or_else(|| eyre::eyre!("q-forming OCOMP ResultVoteV1 is missing"))
}

fn same_locator(left: RequestLocatorV1, right: RequestLocatorV1) -> bool {
    left.intent_id == right.intent_id
        && left.wwd == right.wwd
        && left.pending_nonce == right.pending_nonce
        && left.attempt == right.attempt
        && left.activation_preconditions_hash == right.activation_preconditions_hash
        && left.block_number == right.block_number
        && left.block_hash == right.block_hash
        && left.state_root == right.state_root
}

fn persist_fatal_evidence(
    root: &Path,
    job_id: B256,
    local: B256,
    canonical: B256,
) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join(format!("{}.mismatch-v1", hex::encode(job_id.as_slice())));
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "local_result_digest={local}")?;
    writeln!(file, "canonical_result_digest={canonical}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

const MAX_FATAL_EVIDENCE_BYTES: u64 = 64 * 1024;

fn persist_generic_fatal_evidence(root: &Path, job_id: B256, detail: &str) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join("sticky-fatal-v1");
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "detail={detail}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn load_persisted_fatal_evidence(root: &Path) -> eyre::Result<Option<String>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let Some(path) = paths.into_iter().next() else {
        return Ok(None);
    };
    let file = File::open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_FATAL_EVIDENCE_BYTES {
        bail!("embedded OCOMP fatal evidence has an invalid shape");
    }
    let mut detail = String::new();
    file.take(MAX_FATAL_EVIDENCE_BYTES + 1)
        .read_to_string(&mut detail)?;
    if detail.is_empty() || detail.len() as u64 > MAX_FATAL_EVIDENCE_BYTES {
        bail!("embedded OCOMP fatal evidence has an invalid length");
    }
    Ok(Some(detail))
}

fn persist_local_failure_evidence(root: &Path, job_id: B256, detail: &str) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join(format!("{}.fatal-local-v1", hex::encode(job_id.as_slice())));
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "detail={detail}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn authenticated_finalized_proposer(
    transactions: &[impl reth_primitives_traits::SignedTransaction],
) -> eyre::Result<Address> {
    let transaction = transactions
        .first()
        .ok_or_else(|| eyre::eyre!("finalized block has no proposer-signed system transaction"))?;

    transaction
        .try_recover()
        .wrap_err("recover finalized block proposer from system transaction")
}

fn finalized_materialization_proposer(
    height: u64,
    transactions: &[impl reth_primitives_traits::SignedTransaction],
) -> eyre::Result<Option<Address>> {
    if height == 0 {
        return Ok(None);
    }
    authenticated_finalized_proposer(transactions).map(Some)
}

fn should_wake_nod_materializer(
    policy: EmbeddedNodePolicyV1,
    represented_validator: Option<Address>,
    block_proposer: Address,
    finalized_height: u64,
    head: Option<&outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1>,
    progress_observed: bool,
    retry_interval_blocks: u64,
) -> bool {
    if policy != EmbeddedNodePolicyV1::Validator
        || represented_validator != Some(block_proposer)
        || retry_interval_blocks == 0
    {
        return false;
    }
    let Some(head) = head.filter(|head| head.next_nod_ordinal < head.nod_count) else {
        return false;
    };
    progress_observed
        || finalized_height
            >= head
                .last_progress_height
                .saturating_add(retry_interval_blocks)
}

struct OcompExExStateReaderV1<'a> {
    state: &'a dyn StateProvider,
}

impl StorageReader for OcompExExStateReaderV1<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "OCOMP ExEx finalized state read failed: {error}"
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint_at(block_number: u64, byte: u8) -> OcompExExCheckpointV1 {
        OcompExExCheckpointV1 {
            block_number,
            block_hash: B256::repeat_byte(byte),
        }
    }

    #[test]
    fn canonical_status_matrix_is_exhaustive() {
        assert_eq!(
            classify_canonical_job(OcompJobStatus::AwaitingFinality, false).unwrap(),
            CanonicalJobDispositionV1::AwaitingFinality
        );
        assert_eq!(
            classify_canonical_job(OcompJobStatus::AwaitingFinality, true).unwrap(),
            CanonicalJobDispositionV1::FinalizedAwaitingOpen
        );
        assert_eq!(
            classify_canonical_job(OcompJobStatus::VotingOpen, true).unwrap(),
            CanonicalJobDispositionV1::VotingOpen
        );
        assert_eq!(
            classify_canonical_job(OcompJobStatus::Completed, true).unwrap(),
            CanonicalJobDispositionV1::Completed
        );
        for (status, reason) in [
            (OcompJobStatus::Expired, EmbeddedTerminalReasonV1::Expired),
            (
                OcompJobStatus::Conflicted,
                EmbeddedTerminalReasonV1::Conflicted,
            ),
            (OcompJobStatus::Canceled, EmbeddedTerminalReasonV1::Canceled),
        ] {
            for has_finalized_job in [false, true] {
                assert_eq!(
                    classify_canonical_job(status, has_finalized_job).unwrap(),
                    CanonicalJobDispositionV1::Closed {
                        reason,
                        has_finalized_job,
                    }
                );
            }
        }
        for (status, has_finalized_job) in [
            (OcompJobStatus::VotingOpen, false),
            (OcompJobStatus::Completed, false),
        ] {
            assert!(classify_canonical_job(status, has_finalized_job).is_err());
        }
    }

    #[test]
    fn async_outcome_projection_is_exact_or_retired() {
        let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
        let generation = jobs.observe_job(B256::repeat_byte(0x71), 70).unwrap();
        let other_generation = jobs.observe_job(B256::repeat_byte(0x72), 71).unwrap();
        assert_eq!(
            classify_async_outcome_projection(Some(generation), Some(generation)).unwrap(),
            AsyncOutcomeProjectionV1::Active
        );
        assert_eq!(
            classify_async_outcome_projection(None, None).unwrap(),
            AsyncOutcomeProjectionV1::CheckpointPruned
        );
        for (runtime, reducer) in [
            (Some(generation), None),
            (None, Some(generation)),
            (Some(generation), Some(other_generation)),
        ] {
            assert!(classify_async_outcome_projection(runtime, reducer).is_err());
        }
    }

    #[test]
    fn canonical_restore_policy_never_runs_before_open_or_after_close() {
        for disposition in [
            CanonicalJobDispositionV1::AwaitingFinality,
            CanonicalJobDispositionV1::FinalizedAwaitingOpen,
            CanonicalJobDispositionV1::Closed {
                reason: EmbeddedTerminalReasonV1::Expired,
                has_finalized_job: true,
            },
            CanonicalJobDispositionV1::Closed {
                reason: EmbeddedTerminalReasonV1::Conflicted,
                has_finalized_job: true,
            },
            CanonicalJobDispositionV1::Closed {
                reason: EmbeddedTerminalReasonV1::Canceled,
                has_finalized_job: true,
            },
        ] {
            for policy in [
                EmbeddedNodePolicyV1::Validator,
                EmbeddedNodePolicyV1::FullNode,
            ] {
                assert_eq!(
                    local_result_restore_policy(disposition, policy, true, true, false),
                    LocalResultRestorePolicyV1::Never
                );
            }
        }
    }

    #[test]
    fn canonical_restore_policy_encodes_effect_order() {
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::VotingOpen,
                EmbeddedNodePolicyV1::Validator,
                true,
                false,
                false,
            ),
            LocalResultRestorePolicyV1::BeforeCompute
        );
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::VotingOpen,
                EmbeddedNodePolicyV1::FullNode,
                false,
                false,
                false,
            ),
            LocalResultRestorePolicyV1::BeforeCompute,
            "the FinalizedAwaitingOpen to VotingOpen transition restores before compute"
        );
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::VotingOpen,
                EmbeddedNodePolicyV1::Validator,
                false,
                true,
                true,
            ),
            LocalResultRestorePolicyV1::BeforeCompute
        );
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::Completed,
                EmbeddedNodePolicyV1::FullNode,
                true,
                false,
                false,
            ),
            LocalResultRestorePolicyV1::AfterCanonicalCompleted
        );
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::Completed,
                EmbeddedNodePolicyV1::Validator,
                true,
                true,
                false,
            ),
            LocalResultRestorePolicyV1::Never
        );
        assert_eq!(
            local_result_restore_policy(
                CanonicalJobDispositionV1::VotingOpen,
                EmbeddedNodePolicyV1::FullNode,
                false,
                false,
                true,
            ),
            LocalResultRestorePolicyV1::Never
        );
    }

    #[test]
    fn vote_eligibility_retries_only_unavailable_authority() {
        use outbe_node::ocomp::retention::OcompSnapshotEligibilityV1;

        let pending = advance_vote_eligibility(
            LocalVoteEligibilityV1::Pending,
            OcompSnapshotEligibilityV1::Unavailable {
                detail: "provider lag".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(pending, LocalVoteEligibilityV1::Pending);
        assert_eq!(
            advance_vote_eligibility(pending, OcompSnapshotEligibilityV1::Eligible).unwrap(),
            LocalVoteEligibilityV1::Eligible
        );

        let not_member = advance_vote_eligibility(
            LocalVoteEligibilityV1::Pending,
            OcompSnapshotEligibilityV1::NotMember,
        )
        .unwrap();
        assert_eq!(not_member, LocalVoteEligibilityV1::NotMember);
        assert_eq!(
            advance_vote_eligibility(not_member, OcompSnapshotEligibilityV1::Eligible).unwrap(),
            LocalVoteEligibilityV1::NotMember,
            "exact non-membership is a terminal abstention"
        );
        assert!(advance_vote_eligibility(
            LocalVoteEligibilityV1::Pending,
            OcompSnapshotEligibilityV1::Corrupt {
                detail: "binding mismatch".to_owned(),
            },
        )
        .is_err());
    }

    #[test]
    fn canonical_checkpoint_resumes_in_place() {
        let checkpoint = checkpoint_at(139, 0xaa);
        assert_eq!(
            classify_checkpoint_recovery(checkpoint, Some(checkpoint.block_hash), 139, None)
                .unwrap(),
            CheckpointRecoveryV1::Resume
        );
    }

    /// The watermark is fsynced independently of reth's block persistence, so an
    /// unclean shutdown can leave it above the last written block. Re-scan from
    /// the tip instead of refusing to start.
    #[test]
    fn checkpoint_above_the_canonical_tip_rewinds_and_rescans() {
        let tip_hash = B256::repeat_byte(0xbb);
        assert_eq!(
            classify_checkpoint_recovery(checkpoint_at(139, 0xaa), None, 137, Some(tip_hash))
                .unwrap(),
            CheckpointRecoveryV1::RewindTo(OcompExExCheckpointV1 {
                block_number: 137,
                block_hash: tip_hash,
            })
        );
    }

    /// A hole at or below the tip is not a persistence lag — the history this
    /// node claims to have is inconsistent, so it must not start.
    #[test]
    fn checkpoint_missing_below_the_canonical_tip_fails_closed() {
        let error = classify_checkpoint_recovery(
            checkpoint_at(100, 0xaa),
            None,
            139,
            Some(B256::repeat_byte(0xbb)),
        )
        .expect_err("a gap below the tip is not recoverable");
        assert!(error.to_string().contains("at or below canonical tip"));
    }

    /// A different chain at the same height is divergence, never a rewind.
    #[test]
    fn checkpoint_hash_conflict_fails_closed() {
        let error = classify_checkpoint_recovery(
            checkpoint_at(139, 0xaa),
            Some(B256::repeat_byte(0xcc)),
            139,
            None,
        )
        .expect_err("a conflicting canonical hash is not recoverable");
        assert!(error.to_string().contains("conflicts with canonical"));
    }

    #[test]
    fn unavailable_canonical_tip_fails_closed() {
        let error = classify_checkpoint_recovery(checkpoint_at(139, 0xaa), None, 137, None)
            .expect_err("a missing tip hash is not recoverable");
        assert!(error
            .to_string()
            .contains("canonical tip 137 is unavailable"));
    }

    fn materialization_head(
        last_progress_height: u64,
    ) -> outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
        outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
            queue_sequence: 1,
            job_id: B256::repeat_byte(0x11),
            program_semantics_hash: B256::repeat_byte(0x22),
            worldwide_day: 20_260_812,
            generation: 1,
            nod_root: B256::repeat_byte(0x33),
            nod_count: 10,
            next_nod_ordinal: 8,
            last_progress_height,
        }
    }

    #[test]
    fn finalized_materialization_wake_uses_the_authenticated_system_tx_signer() {
        use outbe_primitives::{
            signer::OutbeEvmSigner,
            system_tx::{build_unsigned_system_tx, SystemTxInputV2},
        };

        let signer = OutbeEvmSigner::from_secret_bytes([0x41; 32]).expect("test signer");
        let input = SystemTxInputV2::CycleTick;
        let unsigned = build_unsigned_system_tx(
            input.kind(),
            0,
            2,
            outbe_primitives::chain::CHAIN_ID,
            input.encode().expect("canonical system input"),
        )
        .expect("unsigned system transaction");
        let signed = signer
            .sign_unsigned(unsigned)
            .expect("signed system transaction");

        let proposer = authenticated_finalized_proposer(&[signed])
            .expect("recover authenticated finalized proposer");
        assert_eq!(proposer, signer.address());
        assert_ne!(
            proposer,
            outbe_primitives::addresses::REWARDS_ADDRESS,
            "the protocol reward beneficiary is not proposer identity"
        );
        assert!(should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(signer.address()),
            proposer,
            100,
            Some(&materialization_head(90)),
            true,
            30,
        ));
    }

    #[test]
    fn genesis_without_system_transactions_has_no_materialization_proposer() {
        let transactions: Vec<outbe_primitives::OutbeTxEnvelope> = Vec::new();
        assert_eq!(
            finalized_materialization_proposer(0, &transactions)
                .expect("genesis has no materialization proposer"),
            None
        );
    }

    #[test]
    fn only_the_validator_representing_the_finalized_proposer_wakes_materialization() {
        let represented = alloy_primitives::Address::repeat_byte(0x11);
        let other = alloy_primitives::Address::repeat_byte(0x22);
        let head = materialization_head(90);

        assert!(should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(represented),
            represented,
            100,
            Some(&head),
            true,
            30,
        ));
        assert!(!should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(represented),
            other,
            100,
            Some(&head),
            true,
            30,
        ));
        assert!(!should_wake_nod_materializer(
            EmbeddedNodePolicyV1::FullNode,
            None,
            represented,
            100,
            Some(&head),
            true,
            30,
        ));
    }

    #[test]
    fn retry_wake_uses_the_resolved_interval_and_requires_an_incomplete_head() {
        let represented = alloy_primitives::Address::repeat_byte(0x11);
        let head = materialization_head(90);

        assert!(!should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(represented),
            represented,
            119,
            Some(&head),
            false,
            30,
        ));
        assert!(should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(represented),
            represented,
            120,
            Some(&head),
            false,
            30,
        ));
        assert!(!should_wake_nod_materializer(
            EmbeddedNodePolicyV1::Validator,
            Some(represented),
            represented,
            120,
            None,
            true,
            30,
        ));
    }

    #[test]
    fn materialization_retry_memory_tracks_only_the_current_head() {
        let old = MaterializationAttemptKeyV1 {
            queue_sequence: 1,
            first_nod_ordinal: 0,
        };
        let current = MaterializationAttemptKeyV1 {
            queue_sequence: 2,
            first_nod_ordinal: 8,
        };
        let mut attempts = BTreeMap::from([(old, 10), (current, 20)]);
        bound_materialization_attempts(&mut attempts, Some(current));
        assert_eq!(attempts, BTreeMap::from([(current, 20)]));
        bound_materialization_attempts(&mut attempts, None);
        assert!(attempts.is_empty());
    }

    #[test]
    fn loaded_checkpoint_waits_for_provider_finality_before_entering_running() {
        let checkpoint_hash = B256::repeat_byte(0x18);

        assert_eq!(
            classify_finalized_head(
                FinalizedHeadPhaseV1::StartupRecovery,
                18,
                checkpoint_hash,
                17,
                B256::repeat_byte(0x17),
            )
            .expect("startup lag is recoverable"),
            FinalizedHeadActionV1::WaitForRecovery
        );
        assert_eq!(
            classify_finalized_head(
                FinalizedHeadPhaseV1::StartupRecovery,
                18,
                checkpoint_hash,
                18,
                checkpoint_hash,
            )
            .expect("exact catch-up enters strict runtime"),
            FinalizedHeadActionV1::EnterRunning
        );
    }

    #[test]
    fn loaded_checkpoint_does_not_publish_ready_before_provider_catches_up() {
        assert_eq!(
            initial_projection_status(
                Some(OcompExExCheckpointV1 {
                    block_number: 18,
                    block_hash: B256::repeat_byte(0x18),
                }),
                B256::repeat_byte(0x01),
            ),
            ProjectionStatus::CatchingUp { checkpoint: None }
        );
    }

    #[test]
    fn checkpoint_hash_conflict_and_post_recovery_regression_remain_fatal() {
        let checkpoint_hash = B256::repeat_byte(0x18);
        let conflict = classify_finalized_head(
            FinalizedHeadPhaseV1::StartupRecovery,
            18,
            checkpoint_hash,
            18,
            B256::repeat_byte(0xff),
        )
        .expect_err("same-height hash conflict must remain fatal");
        assert_eq!(
            conflict,
            "OCOMP finalized head changed hash at the scanned height"
        );

        let regression = classify_finalized_head(
            FinalizedHeadPhaseV1::Running,
            18,
            checkpoint_hash,
            17,
            B256::repeat_byte(0x17),
        )
        .expect_err("runtime regression must remain fatal");
        assert_eq!(
            regression,
            "OCOMP finalized head regressed behind the scanned height"
        );
    }

    #[tokio::test]
    async fn fatal_handoff_keeps_exex_alive_until_node_teardown() {
        assert!(
            tokio::time::timeout(Duration::from_millis(10), wait_for_node_teardown())
                .await
                .is_err(),
            "fatal handoff must not complete the Reth ExEx future"
        );
    }

    #[test]
    fn pre_finalization_request_blocks_checkpoint_until_job_materializes() {
        let intent_id = B256::repeat_byte(0x41);
        let mut materialized = std::collections::BTreeSet::new();
        assert!(!all_requests_materialized(
            [intent_id].into_iter(),
            &materialized
        ));
        materialized.insert(intent_id);
        assert!(all_requests_materialized(
            [intent_id].into_iter(),
            &materialized
        ));
    }

    #[test]
    fn notification_error_closes_the_auxiliary_stream() {
        assert_eq!(
            classify_exex_notification::<(), _>(Some(Err("ce parent mismatch"))),
            ExExNotificationDispositionV1::CloseAfterError("ce parent mismatch")
        );
        assert_eq!(
            classify_exex_notification::<(), &str>(None),
            ExExNotificationDispositionV1::Closed
        );
        assert_eq!(
            classify_exex_notification::<_, &str>(Some(Ok(()))),
            ExExNotificationDispositionV1::Continue
        );
    }

    #[test]
    fn sticky_fatal_evidence_survives_restart_and_is_write_once() {
        let root = tempfile::tempdir().unwrap();
        let job_id = B256::repeat_byte(0x51);
        persist_generic_fatal_evidence(root.path(), job_id, "first fatal").unwrap();
        persist_generic_fatal_evidence(root.path(), B256::repeat_byte(0x52), "later fatal")
            .unwrap();

        let loaded = load_persisted_fatal_evidence(root.path())
            .unwrap()
            .expect("persisted fatal");
        assert!(loaded.contains("first fatal"));
        assert!(!loaded.contains("later fatal"));

        let mismatch_root = tempfile::tempdir().unwrap();
        persist_fatal_evidence(
            mismatch_root.path(),
            job_id,
            B256::repeat_byte(0x61),
            B256::repeat_byte(0x62),
        )
        .unwrap();
        assert!(load_persisted_fatal_evidence(mismatch_root.path())
            .unwrap()
            .expect("mismatch evidence")
            .contains("local_result_digest"));
    }
}
