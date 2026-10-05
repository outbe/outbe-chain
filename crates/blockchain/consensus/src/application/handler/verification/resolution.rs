//! Resolve immutable candidate and parent independently, preserving deterministic rejection.
use super::{ApplicationShared, ResolvedVerifyBlocks, VerifyRequest};
use crate::application::handler::parent_round;
use crate::application::{
    epoch_boundary::{self, EpochBoundaryParentError},
    verify_resolution::{resolve_for_verify, VerifyResolveTarget},
};
use crate::block::ConsensusBlock;
use tracing::warn;

enum ResolutionFailure {
    InvalidParent,
    Unavailable(eyre::Report),
}

impl ApplicationShared {
    pub(super) async fn resolve_verify_blocks(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
    ) -> eyre::Result<Option<ResolvedVerifyBlocks>> {
        // Candidate unavailability cannot override a known invalid parent.
        let resolved = futures::try_join!(
            async {
                Ok::<_, ResolutionFailure>(
                    self.resolve_verification_candidate(clock, request).await,
                )
            },
            self.resolve_verification_parent(clock, request),
        )
        .and_then(|(candidate, parent_block)| {
            candidate.map(|block| ResolvedVerifyBlocks {
                block,
                parent_block,
            })
        });
        match resolved {
            Ok(blocks) => Ok(Some(blocks)),
            Err(ResolutionFailure::InvalidParent) => Ok(None),
            Err(ResolutionFailure::Unavailable(error)) => Err(error),
        }
    }

    async fn resolve_verification_candidate(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
    ) -> Result<ConsensusBlock, ResolutionFailure> {
        let round = request.context.round;
        let block = resolve_for_verify(
            &self.block_cache,
            &self.marshal_mailbox,
            clock,
            round,
            request.payload_digest,
            VerifyResolveTarget::Block,
        )
        .await
        .map_err(|error| {
            ResolutionFailure::Unavailable(eyre::eyre!("candidate unavailable: {error:?}"))
        })?;
        // Persistence outlives parent resolution and response cancellation.
        if self.epoch_fence.check(round, block.number()).is_ok() {
            self.publication.store_candidate(
                &self.marshal_mailbox,
                round,
                std::sync::Arc::new(block.clone()),
            );
        }
        Ok(block)
    }

    async fn resolve_verification_parent(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
    ) -> Result<Option<ConsensusBlock>, ResolutionFailure> {
        let round = request.context.round;
        let parent_view = request.context.parent.0;
        let parent_digest = request.parent_digest();
        match epoch_boundary::resolve_epoch_boundary_parent(
            &self.finalization_view,
            &self.marshal_mailbox,
            clock,
            epoch_boundary::EpochBoundaryParentRequest {
                round,
                parent_view,
                parent_digest,
            },
        )
        .await
        {
            Ok(Some(anchor)) => return Ok(Some(anchor.block)),
            Ok(None) => {}
            Err(EpochBoundaryParentError::ParentMismatch { .. }) => {
                warn!(%round, parent = %parent_digest.0, "verify: epoch boundary parent mismatch with finalized anchor");
                return Err(ResolutionFailure::InvalidParent);
            }
            Err(error) => {
                return Err(ResolutionFailure::Unavailable(eyre::eyre!(
                    "could not resolve epoch boundary parent: {error}"
                )))
            }
        }
        if parent_digest.0 == self.genesis_hash {
            return Ok(None);
        }
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
        .map_err(|error| {
            ResolutionFailure::Unavailable(eyre::eyre!("parent unavailable: {error:?}"))
        })
    }
}
