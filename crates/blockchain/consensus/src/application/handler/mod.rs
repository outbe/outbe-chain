//! Application handler - processes messages from the Automaton/Relay side.
//!
//! This is the "server side" that reads from the mpsc channel populated by
//! [`OutbeApplication`](super::actor::OutbeApplication). It bridges Simplex
//! consensus with Reth's execution layer via `beacon_engine_handle` and
//! `payload_builder_handle`.
//!
//! Block availability uses Commonware's marshal actor:
//! - Proposer disseminates blocks via `buffered::Engine` (broadcast)
//! - Non-proposers resolve blocks via `marshal::resolver` (on-demand P2P)
//! - No ad-hoc block propagation channel or local cache admission

mod dispatch;

use std::sync::Arc;

use outbe_primitives::runtime_audit_v1::{
    process_instance_id, PROPOSAL_VIEW_CANCELLED, SCHEMA_VERSION,
};

// Marshal block-resolution timing constants (FINALIZE_*, VERIFY_RESOLUTION_TIMEOUT,
// PROPOSE_RESOLUTION_TIMEOUT) moved to `crate::config`. The finalization actor, verify,
// and epoch-boundary resolution paths read them cross-module.

use alloy_primitives::Address;
use alloy_primitives::B256;

use commonware_cryptography::bls12381::primitives::variant::MinSig;

use futures::StreamExt;
use outbe_primitives::projection::ExecutionReadBudget;
use outbe_primitives::projection::ProjectionReadinessHandle;
use outbe_primitives::system_tx::OcompLifecycleActivation;
use outbe_primitives::OutbePayloadTypes;

use reth_node_builder::ConsensusEngineHandle;

use reth_payload_builder::PayloadBuilderHandle;
use tracing::debug;
use tracing::error;
use tracing::info;

use crate::ancestry_readiness::AncestryReadiness;
use crate::committee_provider::CommitteeProvider;
use crate::digest::Digest;
use crate::executor;
use crate::finalization::block_cache::BlockCache;
use crate::finalization::state::FinalizationViewAccess;
use crate::finalization::state::FinalizationViewHandle;
use crate::hybrid::election::HybridElectorConfigProvider;
use crate::hybrid::HybridSchemeProvider;
use crate::validators::ValidatorSet;
use crate::vrf_safety::VrfSafetyGate;

use super::ingress::Message;

/// Type alias for engine types used in Outbe.
type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;
type PayloadBuilder = PayloadBuilderHandle<OutbePayloadTypes>;

use crate::application::epoch_boundary::ApplicationEpochFence;

pub struct ApplicationHandler {
    /// Receiver for messages from the Automaton/Relay side.
    rx: futures::channel::mpsc::Receiver<Message>,

    pub(crate) shared: ApplicationShared,
}

#[derive(Clone)]
pub(crate) struct ApplicationShared {
    /// Explicit proposer wall-clock source. Never derived from ambient env.
    unix_time_source: Arc<dyn UnixTimeSource>,

    /// Engine handle for new_payload / fork_choice_updated.
    engine: EngineHandle,

    /// Payload builder handle for building new blocks.
    payload_builder: PayloadBuilder,

    /// Executor mailbox for forwarding finalized blocks.
    executor_mailbox: executor::Mailbox,

    /// Genesis block hash (block 0).
    genesis_hash: B256,

    /// Current active validator set.
    #[allow(dead_code)]
    validators: ValidatorSet,

    /// Active EVM chain id used to rebuild deterministic system-tx envelopes
    /// during consensus prechecks before Engine status is trusted.
    chain_id: u64,

    /// Immutable chain-manifest activation used by the pre-Engine consensus
    /// system-transaction verifier. This must match the payload builder and
    /// execution configuration so every validator expects the same layout at H.
    ocomp_lifecycle_activation: OcompLifecycleActivation,

    /// Marshal mailbox for digest-bound block resolution.
    pub(crate) marshal_mailbox: crate::marshal_types::MarshalMailbox,
    publication: super::publication::ProposalPublication,

    /// Epoch-scoped verifier schemes for carried finalized-parent certificates.
    certificate_scheme_provider: HybridSchemeProvider<MinSig>,

    /// Epoch-scoped leader elector configs. The stateful verify-time elector
    /// use was removed. The field stays on the surface for ctor stability with
    /// the `outbe-engine` stack and for the upcoming V2 verifier hook.
    #[allow(dead_code)]
    elector_config_provider: HybridElectorConfigProvider<MinSig>,

    /// Epoch-scoped ordered committee snapshots.
    committee_provider: CommitteeProvider,

    /// DKG artifact manager for boundary outcomes and dealer logs.
    dkg_manager: crate::dkg_manager::Mailbox,

    /// Fail-closed guard for VRF/DKG freshness.
    vrf_safety: VrfSafetyGate,

    /// DKG activation boundary guard shared with the stack epoch loop.
    epoch_fence: ApplicationEpochFence,

    /// Fast local readiness bit for startup/crash backfill. When false,
    /// ancestry checks fail before they open marshal subscriptions. Those
    /// subscriptions would otherwise wait until timeout while the executor
    /// replays durable consensus blocks into Reth.
    ancestry_readiness: AncestryReadiness,

    /// Exact durable Mongo projection checkpoint used to gate every execution
    /// read of a consensus parent.
    projection_readiness: ProjectionReadinessHandle,

    /// Time to give the payload builder to execute transactions before resolving.
    payload_resolve_time: std::time::Duration,

    /// Proposer-side minimum block-time floor. It is for liveness pacing only
    /// and never affects block contents or validation. The `Message::Propose`
    /// closure reads it via `shared.min_block_time`. Always > 0 (validated at
    /// startup).
    min_block_time: std::time::Duration,

    /// Proposer EVM identity used to sign system transaction artifacts.
    proposer_evm_address: Option<Address>,

    /// Rate limiter for repeated critical proposal failure logs.
    proposal_failure_log_limiter: Arc<crate::util::rate_limit::LogRateLimiter>,

    /// Shared canonical view of the last finalization (forkchoice,
    /// `last_finalized_*`, observed VRF seed, monotonic clock floor).
    /// The FinalizationActor writes it. This handler reads it for `build_block`.
    finalization_view: FinalizationViewHandle,

    /// Shared block cache. The proposer inserts on local build. The
    /// FinalizationActor evicts entries below the new finalized height.
    block_cache: BlockCache,

    /// Proposer-side exact-parent certificate selector.
    ///
    /// It waits for the finalized-parent certificate record matching the
    /// Simplex context parent, then carries the metadata in the Phase 1
    /// begin-zone system transaction rather than in `header.extra_data`.
    finalization_selector: crate::finalization::selection::ParentProofSelector,

    /// Disaster-recovery flag (`--testnet.trust-el-head`). When true and
    /// `FinalizationView` has a non-zero execution head, `handle_genesis`
    /// uses the execution head as the Simplex anchor instead of the chain
    /// genesis hash. This allows blocks to resume from the existing
    /// execution state after a force-DKG restart.
    trust_el_head: bool,

    /// shared late-finalize signature store. On proposal the
    /// handler reads it (`build_artifact`) to pack the in-window
    /// `LateFinalizeCreditsArtifact` into `header.extra_data`. The reporter
    /// writes votes into it and the `FinalizationActor` resolves them. The store
    /// is best-effort and process-local. Every validator re-verifies the
    /// resulting artifact, so the store never affects determinism.
    late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
}

/// Named dependencies for [`ApplicationHandler::new`].
///
/// Replaces a 25-positional-argument constructor, so the single production
/// caller and the test fixtures cannot transpose arguments. The wiring order
/// lives in the type system rather than in a call-site convention. It mirrors
/// `FinalizationActorDeps`, which the same `outbe-engine` call site
/// (`stack::epoch::run`) uses a few lines later.
pub struct ApplicationDeps {
    pub unix_time_source: Arc<dyn UnixTimeSource>,
    pub rx: futures::channel::mpsc::Receiver<Message>,
    pub engine: EngineHandle,
    pub payload_builder: PayloadBuilder,
    pub executor_mailbox: executor::Mailbox,
    pub genesis_hash: B256,
    pub validators: ValidatorSet,
    pub chain_id: u64,
    pub ocomp_lifecycle_activation: OcompLifecycleActivation,
    pub marshal_mailbox: crate::marshal_types::MarshalMailbox,
    pub publication: super::publication::ProposalPublication,
    pub certificate_scheme_provider: HybridSchemeProvider<MinSig>,
    pub elector_config_provider: HybridElectorConfigProvider<MinSig>,
    pub committee_provider: CommitteeProvider,
    pub dkg_manager: crate::dkg_manager::Mailbox,
    pub vrf_safety: VrfSafetyGate,
    pub epoch_fence: ApplicationEpochFence,
    pub ancestry_readiness: AncestryReadiness,
    pub projection_readiness: ProjectionReadinessHandle,
    pub finalization_view: FinalizationViewHandle,
    pub block_cache: BlockCache,
    pub finalization_selector: crate::finalization::selection::ParentProofSelector,
    pub payload_resolve_time: std::time::Duration,
    pub min_block_time: std::time::Duration,
    pub proposer_evm_address: Option<Address>,
    pub trust_el_head: bool,
    pub late_sig_store: crate::finalization::late_sig_store::SharedLateFinalizeStore,
}

impl ApplicationHandler {
    /// Create a new handler.
    ///
    /// The application no longer owns a private finalization view copy.
    /// Forkchoice / last-finalized / prev-randao / monotonic-clock-floor
    /// state lives in the shared [`FinalizationViewHandle`]. The
    /// `FinalizationActor` writes it, and this handler reads it under a
    /// short-lived guard. `new_finalization_view(...)` performs recovery at
    /// the call site in `outbe-engine` (`stack::epoch::run`). This constructor
    /// takes the already-initialized handle.
    pub fn new(deps: ApplicationDeps) -> Self {
        let ApplicationDeps {
            unix_time_source,
            rx,
            engine,
            payload_builder,
            executor_mailbox,
            genesis_hash,
            validators,
            chain_id,
            ocomp_lifecycle_activation,
            marshal_mailbox,
            publication,
            certificate_scheme_provider,
            elector_config_provider,
            committee_provider,
            dkg_manager,
            vrf_safety,
            epoch_fence,
            ancestry_readiness,
            projection_readiness,
            finalization_view,
            block_cache,
            finalization_selector,
            payload_resolve_time,
            min_block_time,
            proposer_evm_address,
            trust_el_head,
            late_sig_store,
        } = deps;
        Self {
            rx,
            shared: ApplicationShared {
                unix_time_source,
                engine,
                payload_builder,
                executor_mailbox,
                genesis_hash,
                validators,
                chain_id,
                ocomp_lifecycle_activation,
                marshal_mailbox,
                publication,
                certificate_scheme_provider,
                elector_config_provider,
                committee_provider,
                dkg_manager,
                vrf_safety,
                epoch_fence,
                ancestry_readiness,
                projection_readiness,
                payload_resolve_time,
                min_block_time,
                proposer_evm_address,
                proposal_failure_log_limiter: Arc::new(
                    crate::util::rate_limit::LogRateLimiter::new(PROPOSAL_FAILURE_LOG_WINDOW),
                ),
                finalization_view,
                block_cache,
                finalization_selector,
                trust_el_head,
                late_sig_store,
            },
        }
    }

    /// Run the handler event loop.
    pub async fn run<E>(mut self, context: E) -> eyre::Result<()>
    where
        E: commonware_runtime::Metrics
            + commonware_runtime::Spawner
            + commonware_runtime::Clock
            + Send
            + Sync
            + 'static,
    {
        info!("application handler started");

        // Step 21: the per-finalization side effects no longer run in
        // this handler. Finalization events flow voter -> OutbeReporter ->
        // FinalizationActor (via the unbounded
        // `finalization::ingress::Mailbox`). The application handler's
        // mailbox handles only Genesis / Propose / Verify.
        loop {
            let msg = match self.rx.next().await {
                Some(m) => m,
                None => {
                    info!("application handler mailbox closed, exiting");
                    return Ok(());
                }
            };
            self.dispatch_message(&context, msg);
        }
    }
}

impl ApplicationShared {
    fn initial_epoch_genesis_digest(&self) -> Digest {
        if self.trust_el_head {
            let anchor = self.finalization_view.finalized_anchor();
            if anchor.number > 0 && anchor.finalized_head_hash != B256::ZERO {
                debug!(
                    finalized_number = anchor.number,
                    finalized_hash = %anchor.finalized_head_hash,
                    "handle_genesis(epoch=0): using execution head as anchor (--testnet.trust-el-head)"
                );
                return Digest(anchor.finalized_head_hash);
            }
        }
        Digest(self.genesis_hash)
    }

    /// Handle genesis request - return the parent digest for `view = 1` of
    /// `genesis.epoch`.
    ///
    /// ** epoch continuity:**
    /// - `epoch == 0` - return the chain genesis hash, as before.
    /// - `epoch > 0` - return the last finalized block's hash from
    ///   `FinalizationView`. That value is the *continuity anchor*: the
    ///   first block produced in the new epoch must extend it.
    ///
    /// Simplex does not call `Automaton::genesis`. The genesis digest feeds
    /// `simplex::Config.floor`. No production code sends `Message::Genesis`,
    /// so only tests reach this handler.
    ///
    /// For `epoch > 0`, the handler waits up to `GENESIS_ANCHOR_WAIT_TIMEOUT`
    /// for the finalization view to publish the anchor. If the wait expires,
    /// it logs `error!` and responds with `B256::ZERO` as a terminal-failure
    /// signal. The bounded wait keeps the handler responsive.
    async fn handle_genesis(
        &self,
        clock: &impl commonware_runtime::Clock,
        genesis: super::ingress::Genesis,
    ) {
        debug!(epoch = %genesis.epoch, "genesis requested");
        let epoch = genesis.epoch;
        if epoch.get() == 0 {
            let _ = genesis.response.send(self.initial_epoch_genesis_digest());
            return;
        }

        // The runtime clock drives this bounded wait, so the deadline and the
        // poll sleep below share one time source. This works on both the tokio
        // and the deterministic runtimes. No wall-clock on the consensus path.
        let deadline = clock.current() + GENESIS_ANCHOR_WAIT_TIMEOUT;
        loop {
            let anchor = self.finalization_view.finalized_anchor();
            let (height, hash) = (anchor.number, anchor.finalized_head_hash);
            if height > 0 && hash != B256::ZERO {
                debug!(
                    %epoch,
                    finalized_number = height,
                    finalized_hash = %hash,
                    "handle_genesis: continuity anchor"
                );
                let _ = genesis.response.send(Digest(hash));
                return;
            }
            if clock.current() >= deadline {
                error!(
                    %epoch,
                    timeout_ms = GENESIS_ANCHOR_WAIT_TIMEOUT.as_millis(),
                    "handle_genesis: epoch>0 without finalized continuity anchor after timeout; \
                     terminal failure - Simplex will lock parent_view=0 to B256::ZERO. \
                     Investigate why FinalizationView lacks last_finalized_number/hash; \
                     stack.rs pre-restart guard should have prevented this."
                );
                let _ = genesis.response.send(Digest(B256::ZERO));
                return;
            }
            clock.sleep(GENESIS_ANCHOR_POLL_INTERVAL).await;
        }
    }
}

#[cfg(test)]
#[path = "../handler_tests.rs"]
mod handler_tests;

#[cfg(test)]
mod clamp_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support;
#[cfg(test)]
use test_support::validate_header_consensus_artifacts;

mod timing;
use timing::{
    pace_and_send, proposal_timestamp_millis, wait_for_projected_parent, ParentProjectionGate,
};
pub use timing::{OffsetUnixTimeSource, SystemUnixTimeSource, UnixTimeSource};
pub(crate) use timing::{
    GENESIS_ANCHOR_POLL_INTERVAL, GENESIS_ANCHOR_WAIT_TIMEOUT, PROPOSAL_FAILURE_LOG_WINDOW,
    VERIFY_SYNCING_RETRY_DELAY,
};

#[cfg(test)]
use timing::{apply_unix_time_offset_millis, clamp_proposed_timestamp_millis, floor_remaining};

mod parent_proof;
pub(crate) use parent_proof::parent_round;
use parent_proof::{finalized_parent_attestation_from_phase1_system_tx, ParentProofLookup};

/// Test-only byzantine proposer for e2e scenarios.
#[cfg(all(
    feature = "e2e-byzantine-preannounce",
    feature = "test-protocol-overrides"
))]
mod byzantine_hook;

mod proposal;
use super::{ancestry, ingress};
#[cfg(test)]
use proposal::{prepare_built_candidate, BuildBlockOutcome};
use proposal::{ProposalPayloadTrace, ProposalRequest, ProposeOutcome};

mod verification;
#[cfg(test)]
use verification::{validate_header_consensus_artifacts_for_activation, ValidatorRole};

#[cfg(test)]
use crate::block::ConsensusBlock;
