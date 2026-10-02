//! Digest-bound proposal/parent resolution and epoch-continuity error classification.
use super::{ApplicationShared, ResolvedVerifyBlocks, VerifyRequest};
use crate::application::handler::parent_round;
use crate::application::{
    epoch_boundary::{self, EpochBoundaryParentError},
    verify_resolution::{resolve_for_verify, VerifyResolveTarget},
};
use commonware_consensus::types::View;
use tracing::warn;

impl ApplicationShared {
    pub(super) async fn resolve_verify_blocks(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
    ) -> eyre::Result<Option<ResolvedVerifyBlocks>> {
        let round = request.context.round;
        let parent_view = request.context.parent.0;
        let parent_digest = request.parent_digest();
        let payload_digest = request.payload_digest;
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
                return Ok(None);
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

        Ok(Some(ResolvedVerifyBlocks {
            block,
            parent_block,
        }))
    }
}
