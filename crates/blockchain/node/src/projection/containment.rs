use super::FinalizedTarget;
use crate::errors::NodeProjectionError;
use alloy_primitives::B256;
use eyre::Context;
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

/// OCOMP-specific interpretation of a durable Mongo projection checkpoint.
///
/// Unlike execution readiness, a projection may be ahead of the finalized job
/// height because Mongo is transport, not authority. Both the projection
/// checkpoint and the requested job identity are still checked against local
/// finalized canonical Reth history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcompProjectionContainment {
    Behind {
        checkpoint: ProjectionCheckpoint,
        required: ProjectionCheckpoint,
    },
    Contains {
        checkpoint: ProjectionCheckpoint,
        required: ProjectionCheckpoint,
    },
}

pub fn ocomp_projection_contains<P>(
    checkpoint: ProjectionCheckpoint,
    required: ProjectionCheckpoint,
    canonical: &P,
) -> eyre::Result<OcompProjectionContainment>
where
    P: BlockHashReader + BlockIdReader,
{
    let finalized = canonical
        .finalized_block_num_hash()
        .wrap_err("read local Reth finality for OCOMP projection containment")?
        .map(|block| FinalizedTarget::new(block.number, block.hash))
        .ok_or(NodeProjectionError::ContainmentNoLocalFinalizedBlock)?;
    let checkpoint_hash = canonical
        .block_hash(checkpoint.block_number)
        .wrap_err("read canonical hash for OCOMP projection checkpoint")?;
    let required_hash = canonical
        .block_hash(required.block_number)
        .wrap_err("read canonical hash for OCOMP finalized job")?;
    evaluate_ocomp_projection_containment(
        checkpoint,
        required,
        finalized,
        checkpoint_hash,
        required_hash,
    )
}

pub(super) fn evaluate_ocomp_projection_containment(
    checkpoint: ProjectionCheckpoint,
    required: ProjectionCheckpoint,
    finalized: FinalizedTarget,
    checkpoint_canonical_hash: Option<B256>,
    required_canonical_hash: Option<B256>,
) -> eyre::Result<OcompProjectionContainment> {
    if checkpoint.block_number > finalized.number {
        return Err(NodeProjectionError::MongoCheckpointNotFinalized {
            checkpoint_number: checkpoint.block_number,
            checkpoint_hash: checkpoint.block_hash,
            finalized_number: finalized.number,
            finalized_hash: finalized.hash,
        }
        .into());
    }
    if required.block_number > finalized.number {
        return Err(NodeProjectionError::JobCheckpointNotFinalized {
            required_number: required.block_number,
            required_hash: required.block_hash,
            finalized_number: finalized.number,
            finalized_hash: finalized.hash,
        }
        .into());
    }
    match checkpoint_canonical_hash {
        Some(hash) if hash == checkpoint.block_hash => {}
        Some(hash) => {
            return Err(NodeProjectionError::MongoCheckpointHashConflict {
                block_number: checkpoint.block_number,
                stored: checkpoint.block_hash,
                canonical: hash,
            }
            .into());
        }
        None => {
            return Err(NodeProjectionError::MongoCheckpointUnavailable {
                block_number: checkpoint.block_number,
                block_hash: checkpoint.block_hash,
            }
            .into());
        }
    }
    match required_canonical_hash {
        Some(hash) if hash == required.block_hash => {}
        Some(hash) => {
            return Err(NodeProjectionError::JobHashConflict {
                block_number: required.block_number,
                requested: required.block_hash,
                canonical: hash,
            }
            .into());
        }
        None => {
            return Err(NodeProjectionError::JobUnavailable {
                block_number: required.block_number,
                block_hash: required.block_hash,
            }
            .into());
        }
    }
    if checkpoint.block_number < required.block_number {
        Ok(OcompProjectionContainment::Behind {
            checkpoint,
            required,
        })
    } else {
        Ok(OcompProjectionContainment::Contains {
            checkpoint,
            required,
        })
    }
}
