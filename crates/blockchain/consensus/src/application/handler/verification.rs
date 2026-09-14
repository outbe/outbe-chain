use super::finalized_parent_attestation_from_phase1_system_tx;

use super::parent_round;
use super::wait_for_projected_parent;
use super::ApplicationShared;
use super::ParentProjectionGate;
use super::VERIFY_SYNCING_RETRY_DELAY;

use crate::application::epoch_boundary;

use crate::application::epoch_boundary::EpochBoundaryParentError;
use crate::application::validation::validate_context_parent_binding;
use crate::application::validation::validate_rewards_beneficiary;
use crate::application::validation::validate_system_tx_leader_binding_for_activation;
use crate::application::verify_resolution::resolve_for_verify;
use crate::application::verify_resolution::VerifyResolveTarget;
use crate::block::ConsensusBlock;
use crate::committee_provider::CommitteeProvider;

use crate::config::VERIFY_RESOLUTION_TIMEOUT;
use crate::digest::Digest;
use crate::dkg_manager::AncestryReader;
use crate::dkg_manager::BoundaryRequirement;

use crate::finalization::state::FinalizationViewAccess;

/// The application handler that processes consensus messages.
///
/// Reads from the mpsc channel and calls `beacon_engine_handle` / `payload_builder_handle`
/// to propose and verify blocks. Finalization side effects are owned by
/// `FinalizationActor`; this handler only observes the finalized view while
/// preparing proposals.
///
/// Block resolution uses marshal's digest-bound model:
/// - `handle_verify()` resolves blocks via `marshal_mailbox.subscribe_by_digest()`
/// - `FinalizationActor` resolves finalized blocks via marshal (or proposer's local cache)
/// - No separate block propagation channel or raw block admission path
///
// `ReplayClassification`, `classify_finalization`,
// `extract_consensus_metadata_from_block`,
// `extract_header_artifact_from_block`, `retry_with_backoff`,
// `RetryFailure`, and `RetryFailureKind` live in
// `crate::finalization::util` (relocated in step 17). After step 21 the
// application handler no longer runs the finalization side effects, so
// it only consumes the metadata + header-artifact extractors on the
// verify path.
use crate::finalization::util::extract_header_artifact_from_block;

use crate::hybrid::HybridSchemeProvider;

use alloy_primitives::Address;

use commonware_consensus::types::Height;
use commonware_consensus::types::Round;
use commonware_consensus::types::View;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_cryptography::bls12381::PublicKey;
use commonware_utils::channel::oneshot;

use outbe_primitives::projection::ExecutionReadBudget;
use outbe_primitives::projection::ProjectionCheckpoint;

use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use outbe_primitives::system_tx::OcompLifecycleActivation;
use outbe_primitives::OutbeExecutionData;

use tracing::debug;

use tracing::warn;

/// Outcome of a `new_payload` execution validation with SYNCING retry. The single
/// source of truth for how a verify path classifies execution status, so the
/// parent and block validations can never drift in their SYNCING/retry policy.
enum PayloadVerification {
    /// Execution accepted the payload. `saw_syncing` is true if SYNCING was
    /// observed before acceptance - in that case the verify request may have been
    /// superseded by a view timeout, so the caller skips its side effects.
    Valid { saw_syncing: bool },
    /// Execution rejected the payload; the caller votes `false`.
    Invalid,
    /// The single-shot verify response channel closed while waiting; the caller
    /// returns without side effects.
    ChannelClosed,
}

// `retry_with_backoff`, `RetryFailure`, `RetryFailureKind` moved to
// `crate::finalization::util` in step 17. Imported at the top of this file.

// `extract_consensus_metadata_from_block` and
// `extract_header_artifact_from_block` moved to
// `crate::finalization::util` in step 17. Imported at the top of this file.

/// Whether the local node validates live proposals. A share-less verifier (a TEE
/// full-node with no proposer EVM address) follows FINALIZED blocks only and skips
/// the leader-binding / DKG-boundary checks (polynomial/DKG-view-dependent, would
/// diverge on a verifier's stale post-rotation state). Replaces a boolean-blind
/// `is_verifier` flag so the role choice is explicit in the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValidatorRole {
    Signer,
    VerifierOnly,
}

impl ValidatorRole {
    /// A node with no proposer EVM address is a share-less verifier-only follower.
    fn from_proposer_evm_address(proposer_evm_address: Option<Address>) -> Self {
        match proposer_evm_address {
            Some(_) => Self::Signer,
            None => Self::VerifierOnly,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn validate_header_consensus_artifacts_for_activation(
    block: &ConsensusBlock,
    parent_block: Option<&ConsensusBlock>,
    round: Round,
    proposer: &PublicKey,
    chain_id: u64,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
    role: ValidatorRole,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
    dkg_manager: &crate::dkg_manager::Mailbox,
    ancestry: &impl AncestryReader,
) -> Result<(), String> {
    // Finalized-follower rule: a share-less verifier (a TEE full-node, no
    // `proposer_evm_address`) does NOT validate live proposals - it follows
    // FINALIZED blocks, whose threshold certificate is verified by the reporter
    // against the GROUP public key (preserved across reshares). The leader-binding
    // and DKG-boundary checks below are polynomial/DKG-view-dependent and would
    // diverge on a verifier's stale post-rotation state, so they are skipped here;
    // consensus safety for the follower comes from the finalization certificate, not
    // from re-deriving the live proposal's leader. The verifier never votes (`me()`
    // is None), so accepting the proposal here cannot affect the committee's quorum.
    if role == ValidatorRole::VerifierOnly {
        return Ok(());
    }
    validate_rewards_beneficiary(block)?;
    validate_system_tx_leader_binding_for_activation(
        block,
        round,
        proposer,
        chain_id,
        ocomp_lifecycle_activation,
        certificate_scheme_provider,
        committee_provider,
    )?;

    let expected_boundary = dkg_manager.pending_boundary_artifact(round.epoch()).await;
    let artifact = extract_header_artifact_from_block(block)?;

    match dkg_manager
        .resolve_boundary(parent_block, expected_boundary.as_ref(), ancestry)
        .await
        .map_err(|error| error.to_string())?
    {
        BoundaryRequirement::NoPending => {}
        BoundaryRequirement::AlreadyCommitted => {
            if matches!(artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(_))) {
                crate::metrics::record_dkg_boundary_duplicate_rejected();
                return Err(
                    "duplicate DKG BoundaryOutcome after parent ancestry already committed it"
                        .to_string(),
                );
            }
            crate::metrics::record_dkg_boundary_requirement(
                crate::metrics::DkgBoundaryDecision::AlreadyCommitted,
            );
            return Ok(());
        }
        BoundaryRequirement::MustEmit => {
            let Some(expected_boundary) = expected_boundary else {
                return Err(
                    "boundary requirement requested emission without pending artifact".to_string(),
                );
            };

            let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) = artifact else {
                return Err("block omitted pending DKG BoundaryOutcome".to_string());
            };
            if boundary != expected_boundary {
                return Err("block BoundaryOutcome does not match pending DKG boundary".to_string());
            }
            return dkg_manager
                .verify_pending_boundary_artifact(round.epoch(), &boundary)
                .await
                .map_err(|error| error.to_string());
        }
    }

    let Some(artifact) = artifact else {
        return Ok(());
    };

    match artifact {
        ConsensusHeaderArtifact::BoundaryOutcome(_) => {
            crate::metrics::record_dkg_boundary_duplicate_rejected();
            Err("block carried DKG BoundaryOutcome without pending boundary".to_string())
        }
        ConsensusHeaderArtifact::DealerLog(bytes) => {
            dkg_manager
                .verify_dealer_log(round.epoch(), bytes.to_vec())
                .await
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome } => {
            // Path A committee pre-announce, emitted during E-1 after the DKG
            // completes. Validate its carried outcome against this node's OWN
            // reconstructed DKG output for the incoming epoch - fail-closed if this
            // node has no pending boundary to compare against, so a forged
            // pre-announce cannot ride a finalized block.
            dkg_manager
                .verify_preannounce_outcome(
                    commonware_consensus::types::Epoch::new(epoch),
                    outcome.as_ref(),
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(())
        }
    }
}

impl ApplicationShared {
    /// Drive `engine.new_payload` to a terminal verdict, retrying while execution
    /// reports SYNCING. Owns the SYNCING/retry policy for both verify paths
    /// (parent and proposed block) so they cannot diverge. Bails to
    /// [`PayloadVerification::ChannelClosed`] if the single-shot verify response
    /// channel closes mid-wait; `kind`/`digest` only scope the diagnostics.
    async fn verify_payload_with_syncing_retry(
        &self,
        clock: &impl commonware_runtime::Clock,
        kind: &'static str,
        digest: Digest,
        execution_data: OutbeExecutionData,
        response: &mut oneshot::Sender<bool>,
        execution_read_budget: &ExecutionReadBudget,
    ) -> eyre::Result<PayloadVerification> {
        let mut saw_syncing = false;
        loop {
            if response.is_closed() {
                execution_read_budget.cancel();
                debug!(
                    kind,
                    target = %digest.0,
                    "verify response channel closed while waiting for execution validation"
                );
                return Ok(PayloadVerification::ChannelClosed);
            }
            let execution = Box::pin(self.engine.new_payload(execution_data.clone()));
            let cancelled = Box::pin(response.closed());
            let status = match futures::future::select(execution, cancelled).await {
                futures::future::Either::Left((status, _)) => status,
                futures::future::Either::Right(((), _)) => {
                    execution_read_budget.cancel();
                    return Ok(PayloadVerification::ChannelClosed);
                }
            };
            match status {
                Ok(status) if status.is_valid() => {
                    debug!(kind, target = %digest.0, ?status, "payload accepted during verify");
                    return Ok(PayloadVerification::Valid { saw_syncing });
                }
                Ok(status) if status.is_syncing() => {
                    saw_syncing = true;
                    warn!(
                        kind,
                        target = %digest.0,
                        ?status,
                        "new_payload returned SYNCING during verify; keeping verification pending until execution validates"
                    );
                    clock.sleep(VERIFY_SYNCING_RETRY_DELAY).await;
                }
                Ok(status) => {
                    warn!(kind, target = %digest.0, ?status, "payload rejected during verify");
                    return Ok(PayloadVerification::Invalid);
                }
                Err(e) => {
                    return Err(eyre::eyre!(
                        "new_payload failed in verify: kind={kind} target={} error={e}",
                        digest.0
                    ));
                }
            }
        }
    }

    /// Handle verify request.
    ///
    /// 1. Resolve proposed block (cache or marshal)
    /// 2. Resolve parent block and send new_payload to Reth
    /// 3. Canonicalize parent
    /// 4. Send new_payload for proposed block
    /// 5. Respond with execution validity
    /// 6. If valid, canonicalize proposed block
    pub(super) async fn handle_verify(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        context: super::ingress::SimplexContext,
        payload_digest: Digest,
        mut response: oneshot::Sender<bool>,
        execution_read_budget: ExecutionReadBudget,
    ) -> eyre::Result<()> {
        let round = context.round;
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);

        debug!(
            %round,
            %parent_view,
            digest = %payload_digest.0,
            parent = %parent_digest.0,
            "verify requested"
        );

        if let Err(rejection) = self.epoch_fence.check(round, 0) {
            debug!(
                %round,
                digest = %payload_digest.0,
                ?rejection,
                "dropping stale verify before block resolution"
            );
            let _ = response.send(false);
            return Ok(());
        }

        // epoch continuity: special-case epoch boundary parent
        // before falling back to chain-genesis / generic verify resolution.
        let maybe_epoch_anchor = match epoch_boundary::resolve_epoch_boundary_parent(
            &self.finalization_view,
            &self.marshal_mailbox,
            clock,
            round,
            parent_view,
            parent_digest,
        )
        .await
        {
            Ok(opt) => opt,
            Err(EpochBoundaryParentError::ParentMismatch { .. }) => {
                // Invalid proposal: proposer chose a parent that does not match
                // the committed continuity anchor. Deterministic reject.
                warn!(
                    %round,
                    parent = %parent_digest.0,
                    "verify: epoch boundary parent mismatch with finalized anchor"
                );
                let _ = response.send(false);
                return Ok(());
            }
            Err(error) => {
                // Local infrastructure issue (missing anchor / marshal miss / hash mismatch).
                // Do NOT vote false - a validator with a temporarily lagging finalization view
                // or marshal store must not reject a block that is in fact valid. Bubble Err
                // so the response channel drops, matching existing `resolve_for_verify`
                // behaviour for local timeouts.
                return Err(eyre::eyre!(
                    "could not resolve epoch boundary parent: {error}"
                ));
            }
        };
        debug_assert!(
            !(round.epoch().get() > 0
                && parent_view == View::new(0)
                && maybe_epoch_anchor.is_none()),
            "resolve_epoch_boundary_parent invariant: epoch>0 && parent_view=0 must \
             resolve to Some(EpochBoundaryParent) or return an explicit error"
        );

        let block_resolution = resolve_for_verify(
            &self.block_cache,
            &self.marshal_mailbox,
            clock,
            round,
            payload_digest,
            VerifyResolveTarget::Block,
        );
        let parent_resolution = async {
            if let Some(anchor) = maybe_epoch_anchor {
                Ok(Some(anchor.block))
            } else if parent_digest.0 == self.genesis_hash {
                Ok(None)
            } else {
                resolve_for_verify(
                    &self.block_cache,
                    &self.marshal_mailbox,
                    clock,
                    parent_round(round, parent_view),
                    parent_digest,
                    VerifyResolveTarget::Parent,
                )
                .await
                .map(Some)
            }
        };

        // `futures::try_join!` is runtime-agnostic (no tokio reactor needed); it polls
        // both resolutions concurrently and short-circuits on the first `Err`,
        // identical to the prior `tokio::try_join!`.
        let (block, parent_block) = match futures::try_join!(block_resolution, parent_resolution) {
            Ok(result) => result,
            Err(error) => {
                return Err(eyre::eyre!(
                    "failed to resolve verify payload or parent: error={error:?} round={round} digest={} parent={}",
                    payload_digest.0,
                    parent_digest.0
                ));
            }
        };

        if let Err(error) = validate_context_parent_binding(
            &block,
            parent_block.as_ref(),
            parent_digest,
            self.genesis_hash,
        ) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                parent = %parent_digest.0,
                %error,
                "proposed block does not extend Simplex context parent"
            );
            let _ = response.send(false);
            return Ok(());
        }

        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "dropping stale verify before Engine API work"
            );
            let _ = response.send(false);
            return Ok(());
        }

        if let Err(error) = self.vrf_safety.ensure_block_allowed(block.number()) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                %error,
                "proposed block is above VRF expiry"
            );
            let _ = response.send(false);
            return Ok(());
        }

        let ancestry = super::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            Some(round),
            VERIFY_RESOLUTION_TIMEOUT,
            clock.child("ancestry"),
        );
        if let Err(error) = validate_header_consensus_artifacts_for_activation(
            &block,
            parent_block.as_ref(),
            round,
            &context.leader,
            self.chain_id,
            self.ocomp_lifecycle_activation,
            ValidatorRole::from_proposer_evm_address(self.proposer_evm_address),
            &self.certificate_scheme_provider,
            &self.committee_provider,
            &self.dkg_manager,
            &ancestry,
        )
        .await
        {
            if error.contains("DKG boundary ancestry unavailable")
                || error.contains("DKG boundary ancestry scan exceeded")
            {
                crate::metrics::record_dkg_boundary_unavailable(
                    crate::metrics::DkgBoundaryUnavailableReason::AncestryUnavailable,
                );
                return Err(eyre::eyre!("DKG boundary requirement unavailable: {error}"));
            }
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "proposed block carries invalid header consensus artifact"
            );
            let _ = response.send(false);
            return Ok(());
        }

        // `handle_verify` performs ONLY structural
        // checks - Phase 1 system tx decode succeeds, header artifacts well-
        // formed, parent binding correct, VRF window not expired. It does NOT
        // perform BLS decode/verify on the carried certificate, does not
        // perform accounting checks, and does not look up committee snapshots.
        // The full V2 cryptographic verify is delegated to the EVM-side V2
        // verifier (`outbe-consensus-proof::verify_v2_proof`, consumed by
        // class verifier wiring), keeping `handle_verify` cheap and
        // stateless across every validator.
        if let Err(error) = finalized_parent_attestation_from_phase1_system_tx(&block) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "failed to decode Phase 1 finalized-parent metadata structure during verify"
            );
            let _ = response.send(false);
            return Ok(());
        }

        let required_parent = ProjectionCheckpoint {
            block_number: parent_block.as_ref().map_or(0, ConsensusBlock::number),
            block_hash: parent_digest.0,
        };
        let projection_budget = execution_read_budget.clone();
        if wait_for_projected_parent(self.projection_readiness.clone(), required_parent, async {
            response.closed().await;
            projection_budget.cancel();
        })
        .await?
            == ParentProjectionGate::Withhold
        {
            return Ok(());
        }

        if let Some(parent_block) = parent_block {
            let parent_height = Height::new(parent_block.number());
            let execution_data =
                OutbeExecutionData::new(std::sync::Arc::new(parent_block.clone().into_inner()))
                    .with_execution_read_budget(execution_read_budget.clone());

            let parent_saw_syncing =
                if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                    warn!(
                        height = %parent_height,
                        parent = %parent_digest.0,
                        "test-marshal-drop: skipping verify parent new_payload"
                    );
                    false
                } else {
                    match self
                        .verify_payload_with_syncing_retry(
                            clock,
                            "parent",
                            parent_digest,
                            execution_data,
                            &mut response,
                            &execution_read_budget,
                        )
                        .await?
                    {
                        PayloadVerification::ChannelClosed => return Ok(()),
                        PayloadVerification::Invalid => {
                            let _ = response.send(false);
                            return Ok(());
                        }
                        PayloadVerification::Valid { saw_syncing } => saw_syncing,
                    }
                };

            if response.is_closed() || parent_saw_syncing {
                debug!(
                    parent = %parent_digest.0,
                    parent_saw_syncing,
                    "skipping verify parent side effects after pending/cancelable execution validation"
                );
            } else if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
                debug!(
                    %round,
                    parent = %parent_digest.0,
                    block_number = block.number(),
                    ?rejection,
                    "skipping verify parent side effects after stale epoch transition"
                );
            } else {
                if let Err(e) = self
                    .executor_mailbox
                    .canonicalize_head(parent_height, parent_digest)
                    .await
                {
                    return Err(eyre::eyre!(
                        "canonicalize_head failed for parent during verify: parent={} error={e}",
                        parent_digest.0
                    ));
                }

                self.finalization_view
                    .advance_timestamp_floor(parent_block.timestamp_millis());
            }
        }

        // Step 3: new_payload for proposed block.
        let execution_data =
            OutbeExecutionData::new(std::sync::Arc::new(block.clone().into_inner()))
                .with_execution_read_budget(execution_read_budget.clone());

        let block_height = Height::new(block.number());
        let (valid, block_saw_syncing) =
            if crate::test_faults::should_drop_new_payload_for_test(block_height) {
                warn!(
                    height = %block_height,
                    digest = %payload_digest.0,
                    "test-marshal-drop: skipping verify block new_payload"
                );
                (true, false)
            } else {
                match self
                    .verify_payload_with_syncing_retry(
                        clock,
                        "block",
                        payload_digest,
                        execution_data,
                        &mut response,
                        &execution_read_budget,
                    )
                    .await?
                {
                    PayloadVerification::ChannelClosed => return Ok(()),
                    PayloadVerification::Invalid => (false, false),
                    PayloadVerification::Valid { saw_syncing } => (true, saw_syncing),
                }
            };

        // Step 4: If valid, canonicalize the proposed block only while the
        // single-shot Simplex verify request is still live. A SYNCING retry can
        // outlive the view timeout; once the receiver is gone, all side effects
        // for this verify request must be suppressed.
        if !valid {
            let _ = response.send(false);
            return Ok(());
        }
        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "skipping verify block side effects after stale epoch transition"
            );
            return Ok(());
        }
        if response.is_closed() {
            debug!(
                digest = %payload_digest.0,
                "verify response channel closed before execution-valid side effects"
            );
            return Ok(());
        }
        if response.send(true).is_err() {
            debug!(
                digest = %payload_digest.0,
                "verify response receiver dropped before execution-valid side effects"
            );
            return Ok(());
        }
        if block_saw_syncing {
            debug!(
                digest = %payload_digest.0,
                "skipping verify block side effects after pending/cancelable execution validation"
            );
            return Ok(());
        }
        let _ = self.marshal_mailbox.verified(round, block.clone()).await;
        let _ = self
            .executor_mailbox
            .canonicalize_head(block_height, payload_digest)
            .await;
        self.finalization_view
            .advance_timestamp_floor(block.timestamp_millis());

        Ok(())
    }
}
