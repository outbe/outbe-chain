//! Stateless system-transaction header and body bindings, in rejection order.
use super::{ConsensusError, OutbeBlockBody, OutbeHeader, REWARDS_ADDRESS};
use alloy_consensus::BlockHeader as _;
use outbe_primitives::{
    block::BlockOrder,
    reshare_artifact::ConsensusHeaderArtifact,
    system_tx::{
        binding::{self, BoundaryBindingError, PublicationBindingError},
        CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX, CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
    },
};
use std::sync::Arc;

pub(super) fn validate_beneficiary(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let regime = outbe_primitives::block::BlockHeightRegime::for_block(header.number());
    if !regime.is_genesis() && header.beneficiary() != REWARDS_ADDRESS {
        return Err(
            crate::errors::ConsensusPolicyError::NonGenesisBeneficiaryMismatch {
                expected: REWARDS_ADDRESS,
                actual: header.beneficiary(),
            }
            .into(),
        );
    }

    Ok(())
}

pub(super) fn validate_layout(
    body: &OutbeBlockBody,
    header: &OutbeHeader,
    block_order: &BlockOrder,
) -> Result<(), ConsensusError> {
    let (layout, artifacts) = binding::validate_system_layout(body, header, block_order)
        .map_err(|error| ConsensusError::Other(Arc::new(error)))?;
    if matches!(
        artifacts.consensus_header_artifact,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
    ) && !layout.has_consensus_step(CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX)
    {
        return Err(ConsensusError::Other(Arc::new(
            BoundaryBindingError::Missing,
        )));
    }
    if artifacts.radicle_publication.is_some()
        && !layout.has_consensus_step(CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX)
    {
        return Err(ConsensusError::Other(Arc::new(
            PublicationBindingError::Missing,
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
