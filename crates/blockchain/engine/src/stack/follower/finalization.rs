use super::*;
use commonware_codec::Encode as _;
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::FinalizedParentCertStore,
    marshal_types::{Finalization, MarshalMailbox},
};

pub(super) async fn finalization_bytes_for_height(
    mailbox: &MarshalMailbox,
    parent_store: &FinalizedParentCertStore,
    height: Height,
) -> Option<outbe_primitives::consensus::FinalizedBlockBytes> {
    let (_, digest) = mailbox.get_info(height).await?;
    let block = mailbox.get_block(&digest).await?;
    let finalization = match mailbox.get_finalization(height).await {
        Some(local) => Some(local),
        None => match same_block_finalization(parent_store, height, &block, digest) {
            Some(retained) => Some(retained),
            None => child_finalization(mailbox, height, &block).await,
        },
    };
    Some(outbe_primitives::consensus::FinalizedBlockBytes {
        finalization: finalization
            .map(|value| alloy_primitives::Bytes::from(value.encode().to_vec()))
            .unwrap_or_default(),
        block: alloy_primitives::Bytes::from(block.encode().to_vec()),
    })
}
fn same_block_finalization(
    parent_store: &FinalizedParentCertStore,
    height: Height,
    block: &ConsensusBlock,
    digest: Digest,
) -> Option<Finalization> {
    for record in parent_store.finalizations_for_block(height.get(), block.block_hash()) {
        if let Ok(candidate) =
            outbe_consensus::follow::decode_public_finalization(&record.encoded_proof, 256)
        {
            if candidate.proposal.payload == digest {
                return Some(candidate);
            }
        }
    }
    None
}
async fn child_finalization(
    mailbox: &MarshalMailbox,
    height: Height,
    block: &ConsensusBlock,
) -> Option<Finalization> {
    let next = Height::new(height.get().checked_add(1)?);
    let (_, child_digest) = mailbox.get_info(next).await?;
    let child = mailbox.get_block(&child_digest).await?;
    outbe_consensus::follow::upstream::parent_finalization_from_child(block, &child)
}
