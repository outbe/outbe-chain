//! Embedded OCOMP observer and local FullNode readiness gate.
//!
//! Finalized chain state remains the only authority. ExEx notifications are
//! drained for Reth backpressure, while the provider's finalized head selects
//! the exact canonical range processed here.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;

use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::discovery_control::DiscoveryOfferRefV1;
use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_ocomp::discovery_spool::DiscoverySpoolV1;
use outbe_ocomp::embedded::EmbeddedJobGenerationV1;
use outbe_ocomp::embedded::EmbeddedOcompJobsV1;
use outbe_ocomp::embedded_runtime::EmbeddedComputeOutcomeV1;
use outbe_ocomp::embedded_runtime::EmbeddedMaterializationOutcomeV1;
use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompDomainV1;
use outbe_ocomp::embedded_runtime::EmbeddedPayoutOutcomeV1;
use outbe_ocomp::embedded_runtime::EmbeddedVoteOutcomeV1;
use outbe_ocomp::supervisor::DiscoveryRecord;

use outbe_ocomp_protocol::local_control::EndpointIdentity;
use outbe_ocomp_protocol::result::LysisResultV1;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionFailure;
use outbe_primitives::projection::ProjectionFailureClass;
use outbe_primitives::projection::ProjectionReadinessPublisher;
use outbe_primitives::projection::ProjectionStatus;

/// Days each embedded payout tick looks back over.
const PAYOUT_LOOKBACK_DAYS: u32 = 30;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The drain and finalized reader share one publication order. Once either
/// reports a fatal failure, an in-flight reader tick cannot overwrite it.
#[derive(Clone)]
struct OcompReadinessV1(Arc<std::sync::Mutex<ProjectionReadinessPublisher>>);

impl OcompReadinessV1 {
    fn publish(&self, status: ProjectionStatus) {
        match self.0.lock() {
            Ok(publisher) => {
                if !matches!(publisher.current(), ProjectionStatus::Fatal { .. }) {
                    publisher.publish(status);
                }
            }
            Err(poisoned) => poisoned.into_inner().publish(ProjectionStatus::Fatal {
                checkpoint: None,
                error: ProjectionFailure::new(
                    ProjectionFailureClass::Other,
                    "OCOMP readiness publication lock is poisoned",
                ),
            }),
        }
    }
}

#[derive(Clone)]
pub struct OcompExExBundleConfigV1 {
    pub worker_address: std::net::SocketAddr,
    pub identity: EndpointIdentity,
    pub protocol_bundle: PinnedProtocolBundle,
}

#[derive(Clone)]
pub struct OcompExExConfigV1 {
    pub domain_root: PathBuf,
    pub discovery_spool_root: PathBuf,
    pub bundles: Vec<OcompExExBundleConfigV1>,
    pub policy: EmbeddedNodePolicyV1,
    pub validator_rpc_url: Option<String>,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub retention_selector: Arc<outbe_node::ocomp::retention::SharedOcompRetentionSelector>,
    pub retention_required: bool,
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
    before_request: ProjectionCheckpoint,
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

struct EmbeddedOcompExExV1<P> {
    provider: P,
    policy: EmbeddedNodePolicyV1,
    domain: EmbeddedOcompDomainV1,
    readiness: OcompReadinessV1,
    exit: tokio::sync::mpsc::UnboundedSender<OcompExExExitV1>,
    requests: BTreeMap<B256, RequestLocatorV1>,
    materialized_requests: BTreeSet<B256>,
    jobs: BTreeMap<B256, RuntimeJobV1>,
    intent_jobs: BTreeMap<B256, B256>,
    discovery_spools: BTreeMap<B256, DiscoverySpoolV1>,
    pending_offers: BTreeMap<B256, DiscoveryOfferRefV1>,
    acknowledged_exports: BTreeSet<B256>,
    retention_selector: Arc<outbe_node::ocomp::retention::SharedOcompRetentionSelector>,
    closure_checkpoint: ContiguousCheckpointStoreV1,
    latest_scanned_checkpoint: ProjectionCheckpoint,
    state: EmbeddedOcompJobsV1,
    scanned_height: u64,
    scanned_hash: B256,
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

#[cfg(test)]
mod tests;

mod canonical;
use canonical::{
    advance_vote_eligibility, classify_async_outcome_projection, classify_canonical_job,
    ignored_compute_result_reason, local_result_restore_policy,
    released_export_authority_for_status, request_projection_is_closed, same_locator,
    AsyncOutcomeProjectionV1, CanonicalJobDispositionV1, LocalResultRestorePolicyV1,
};

mod failure;
use failure::{
    consume_projection_runtime_deadline, drain_exex_notifications, load_persisted_fatal_evidence,
    persist_local_failure_evidence, projection_runtime_failure,
    projection_runtime_watch_closed_failure, projection_task_failure, publish_finished_height,
    without_execution_backfill,
};

#[cfg(test)]
use failure::{persist_fatal_evidence, persist_generic_fatal_evidence};

mod discovery;
use discovery::{
    discovery_record, publish_finalized_reader_metrics, record_discovery_retirement_report,
    OcompExExStateReaderV1,
};

#[cfg(test)]
use discovery::finalized_reader_lags;

mod retention;
use retention::{
    classify_retention_reconciliation, retention_runtime_error_requires_frame_retry,
    RetentionReconciliationDispositionV1,
};

mod compute;

mod vote;

mod materialization;
#[cfg(test)]
use materialization::{
    authenticated_finalized_proposer, finalized_materialization_proposer,
    should_wake_nod_materializer,
};

mod payout;

mod run;
pub use run::run_ocomp_exex;
#[cfg(test)]
use run::wait_for_node_teardown;
