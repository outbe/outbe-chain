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
    #[error("invalid repository publication anchor: {0}")]
    Invalid(&'static str),
    #[error("repository publication finalized history is locally unavailable")]
    Unavailable,
}

pub(crate) async fn validate_publication_anchor(
    block: &ConsensusBlock,
    chain_id: u64,
    genesis_hash: B256,
    reader: &impl FinalizedBlockReader,
) -> Result<(), PublicationAnchorError> {
    use PublicationAnchorError::{Invalid, Unavailable};
    let extra_data = block.header().inner.extra_data.as_ref();
    // Node policy binds publication mode to genesis. Decode every OART envelope before reading records.
    if !extra_data.starts_with(b"OART") {
        return Ok(());
    }
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(extra_data)
        .map_err(|_| Invalid("malformed header artifacts"))?;
    let Some(batch) = artifacts.radicle_publication else {
        return Ok(());
    };
    let body = &batch.body;
    if body.chain_id != chain_id
        || body.genesis_hash != genesis_hash
        || body.finalized_anchor_height >= block.number()
        || body.finalized_anchor_hash.is_zero()
    {
        return Err(Invalid("chain or height"));
    }
    if body.finalized_anchor_height == 0 {
        return if body.finalized_anchor_hash == genesis_hash {
            Ok(())
        } else {
            Err(Invalid("genesis hash"))
        };
    }
    let finalized = reader
        .finalized_block(body.finalized_anchor_height)
        .await
        .ok_or(Unavailable)?;
    if finalized.number() != body.finalized_anchor_height {
        return Err(Unavailable);
    }
    if finalized.block_hash() != body.finalized_anchor_hash {
        return Err(Invalid("hash differs from canonical finalized history"));
    }
    Ok(())
}
