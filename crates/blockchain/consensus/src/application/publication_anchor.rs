//! Admission against the node's authenticated finalized history, never a proposal cache.

use crate::{block::ConsensusBlock, dkg_manager::BlockLookupFuture};
use alloy_primitives::B256;

pub(crate) trait FinalizedBlockReader {
    /// Return only the canonical finalized block at this height.
    fn finalized_block(&self, height: u64) -> BlockLookupFuture<'_>;
}

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PublicationAnchorError {
    #[error("invalid repository publication anchor: malformed header artifacts")]
    MalformedHeaderArtifacts(#[source] outbe_primitives::errors::PrecompileError),
    #[error("invalid repository publication anchor: chain or height")]
    ChainOrHeightMismatch,
    #[error("invalid repository publication anchor: genesis hash")]
    GenesisHashMismatch,
    #[error(
        "invalid repository publication anchor: hash differs from canonical finalized history"
    )]
    FinalizedHashMismatch,
    #[error("repository publication finalized history is locally unavailable")]
    Unavailable,
}

pub(crate) async fn validate_publication_anchor(
    block: &ConsensusBlock,
    chain_id: u64,
    genesis_hash: B256,
    reader: &impl FinalizedBlockReader,
) -> Result<(), PublicationAnchorError> {
    use PublicationAnchorError::{
        ChainOrHeightMismatch, FinalizedHashMismatch, GenesisHashMismatch,
        MalformedHeaderArtifacts, Unavailable,
    };
    let extra_data = block.header().inner.extra_data.as_ref();
    // Node policy binds publication mode to genesis. Decode every OART envelope before reading records.
    if !extra_data.starts_with(b"OART") {
        return Ok(());
    }
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(extra_data)
        .map_err(MalformedHeaderArtifacts)?;
    let Some(batch) = artifacts.radicle_publication else {
        return Ok(());
    };
    let body = &batch.body;
    if body.chain_id != chain_id
        || body.genesis_hash != genesis_hash
        || body.finalized_anchor_height >= block.number()
        || body.finalized_anchor_hash.is_zero()
    {
        return Err(ChainOrHeightMismatch);
    }
    if body.finalized_anchor_height == 0 {
        return if body.finalized_anchor_hash == genesis_hash {
            Ok(())
        } else {
            Err(GenesisHashMismatch)
        };
    }
    let finalized = reader
        .finalized_block(body.finalized_anchor_height)
        .await
        .ok_or(Unavailable)?;
    if finalized.number() != body.finalized_anchor_height {
        Err(Unavailable)
    } else if finalized.block_hash() != body.finalized_anchor_hash {
        Err(FinalizedHashMismatch)
    } else {
        Ok(())
    }
}
