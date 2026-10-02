//! Verification stages preserve the distinction between rejection and local unavailability.
use super::{
    wait_for_projected_parent, ApplicationShared, ParentProjectionGate, VERIFY_SYNCING_RETRY_DELAY,
};
use crate::{
    application::{
        ingress::SimplexContext,
        validation::{
            validate_rewards_beneficiary, validate_system_tx_leader_binding_for_activation,
        },
    },
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    dkg_manager::{AncestryReader, ArtifactAdmissionError, BoundaryRequirement},
    finalization::util::extract_header_artifact_from_block,
    hybrid::HybridSchemeProvider,
};
use alloy_primitives::Address;
use commonware_consensus::types::Round;
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PublicKey};
use commonware_utils::channel::oneshot;
use outbe_primitives::{
    projection::{ExecutionReadBudget, ProjectionCheckpoint},
    system_tx::OcompLifecycleActivation,
    OutbeExecutionData,
};
use tracing::{debug, warn};

mod execution;
mod prechecks;
mod resolution;
mod verdict;

/// Request identity stays in the canonical Simplex context throughout every stage.
struct VerifyRequest {
    context: SimplexContext,
    payload_digest: Digest,
}

impl VerifyRequest {
    fn parent_digest(&self) -> Digest {
        self.context.parent.1
    }
}

struct ResolvedVerifyBlocks {
    block: ConsensusBlock,
    parent_block: Option<ConsensusBlock>,
}

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

/// Facts carried by one proposal being checked for header-artifact admission.
pub(super) struct HeaderArtifactRequest<'a> {
    pub(super) block: &'a ConsensusBlock,
    pub(super) parent_block: Option<&'a ConsensusBlock>,
    pub(super) round: Round,
    pub(super) proposer: &'a PublicKey,
    pub(super) role: ValidatorRole,
}

/// Existing chain policy and read-only admission dependencies, borrowed for the request.
pub(super) struct HeaderArtifactValidationDeps<'a, A: AncestryReader> {
    pub(super) chain_id: u64,
    pub(super) ocomp_lifecycle_activation: OcompLifecycleActivation,
    pub(super) certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub(super) committee_provider: &'a CommitteeProvider,
    pub(super) dkg_manager: &'a crate::dkg_manager::Mailbox,
    pub(super) ancestry: &'a A,
}

pub(super) async fn validate_header_consensus_artifacts_for_activation(
    request: HeaderArtifactRequest<'_>,
    deps: HeaderArtifactValidationDeps<'_, impl AncestryReader>,
) -> Result<(), ArtifactAdmissionError> {
    let HeaderArtifactRequest {
        block,
        parent_block,
        round,
        proposer,
        role,
    } = request;
    let HeaderArtifactValidationDeps {
        chain_id,
        ocomp_lifecycle_activation,
        certificate_scheme_provider,
        committee_provider,
        dkg_manager,
        ancestry,
    } = deps;
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
    validate_rewards_beneficiary(block).map_err(ArtifactAdmissionError::Rejected)?;
    validate_system_tx_leader_binding_for_activation(
        block,
        round,
        proposer,
        chain_id,
        ocomp_lifecycle_activation,
        certificate_scheme_provider,
        committee_provider,
    )
    .map_err(ArtifactAdmissionError::Rejected)?;

    let artifact =
        extract_header_artifact_from_block(block).map_err(ArtifactAdmissionError::Rejected)?;
    let requirement = dkg_manager
        .admit_header_artifact(parent_block, round.epoch(), artifact.as_ref(), ancestry)
        .await?;
    if requirement == BoundaryRequirement::AlreadyCommitted {
        crate::metrics::record_dkg_boundary_requirement(
            crate::metrics::DkgBoundaryDecision::AlreadyCommitted,
        );
    }
    Ok(())
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

        let request = VerifyRequest {
            context,
            payload_digest,
        };
        let Some(resolved) = self.resolve_verify_blocks(clock, &request).await? else {
            let _ = response.send(false);
            return Ok(());
        };
        if !self
            .validate_verify_blocks(clock, &request, &resolved)
            .await?
        {
            let _ = response.send(false);
            return Ok(());
        }

        let required_parent = ProjectionCheckpoint {
            block_number: resolved
                .parent_block
                .as_ref()
                .map_or(0, ConsensusBlock::number),
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

        match self
            .verify_parent_execution(
                clock,
                &request,
                &resolved,
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
            PayloadVerification::Valid { .. } => {}
        }
        let outcome = self
            .verify_block_execution(
                clock,
                &request,
                &resolved.block,
                &mut response,
                &execution_read_budget,
            )
            .await?;
        self.publish_verify_verdict(&request, &resolved.block, response, outcome)
            .await
    }
}
