//! Public finality envelope decoding and validation.

use crate::{
    block::ConsensusBlock, bls::MAX_COMMITTEE_MEMBERS, errors::FollowError,
    marshal_types::Finalization,
};
use commonware_codec::Read as _;
use commonware_consensus::types::Height;
use std::fmt;

/// Recover an exact direct certificate from the child's on-chain accounting
/// transaction. A certified notarization is deliberately not interchangeable
/// with a finalization. This only extracts evidence: the consumer still must
/// verify the certificate against the historically authenticated committee.
pub fn parent_finalization_from_child(
    parent: &ConsensusBlock,
    child: &ConsensusBlock,
) -> Option<Finalization> {
    use alloy_consensus::Transaction as _;
    use outbe_primitives::{consensus_metadata::ParentParticipationProof, system_tx::SystemTx};
    if parent.number().checked_add(1)? != child.number() || child.parent_digest() != parent.digest()
    {
        return None;
    }
    for transaction in &child.body().transactions {
        let Ok(SystemTx::CertifiedParentAccounting { metadata }) =
            SystemTx::decode(transaction.input().as_ref())
        else {
            continue;
        };
        if metadata.proof_kind != ParentParticipationProof::Finalization
            || metadata.finalized_block_number != parent.number()
            || metadata.finalized_block_hash != parent.block_hash()
        {
            continue;
        }
        let certificate =
            decode_public_finalization(&metadata.proof, MAX_COMMITTEE_MEMBERS).ok()?;
        if certificate.proposal.payload == parent.digest()
            && certificate.proposal.round.epoch().get() == metadata.finalized_epoch
        {
            return Some(certificate);
        }
    }
    None
}

/// A finalized block together with the finalization certificate that proves it.
///
/// The certificate is NOT trusted by the transport. The marshal re-verifies it
/// against the epoch committee before the block is accepted. The resolver
/// (through `CommitteeChain` admission) and the engine bootstrap register that
/// committee. The driver does not.
#[derive(Clone)]
pub struct CertifiedFinalizedBlock {
    /// The finalization certificate for this height (committee-bound).
    pub finalization: Finalization,
    /// The finalized consensus block.
    pub block: ConsensusBlock,
}

/// Direct finality at `certified`, with an optional ascending chain of ancestors.
#[derive(Clone)]
pub struct AncestorFinalityProof {
    pub certified: CertifiedFinalizedBlock,
    pub ancestors: Vec<ConsensusBlock>,
}

impl AncestorFinalityProof {
    pub fn target(&self) -> &ConsensusBlock {
        self.ancestors.first().unwrap_or(&self.certified.block)
    }

    pub fn validate_envelope(&self, height: Height) -> eyre::Result<()> {
        if self.ancestors.len() > 64 {
            return Err(FollowError::TooManyFinalityAncestors.into());
        }
        if self.target().number() != height.get() {
            return Err(FollowError::CertifiedBlockHeightMismatch {
                actual: self.target().number(),
                expected: height.get(),
            }
            .into());
        }
        if self.certified.finalization.proposal.payload != self.certified.block.digest() {
            return Err(FollowError::FinalizationPayloadMismatch {
                height: self.certified.block.number(),
            }
            .into());
        }
        let mut previous = self.target();
        for child in self
            .ancestors
            .iter()
            .skip(1)
            .chain(std::iter::once(&self.certified.block))
        {
            if self.ancestors.is_empty() {
                break;
            }
            if previous.number().checked_add(1) != Some(child.number())
                || child.parent_digest() != previous.digest()
            {
                return Err(FollowError::InvalidFinalityAncestorChain.into());
            }
            previous = child;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublicFinalizedBlockDecodeError {
    #[error("invalid Commonware finalization bytes: {0}")]
    Finalization(String),
    #[error("trailing bytes after Commonware finalization")]
    TrailingFinalization,
    #[error("invalid finalized consensus block bytes: {0}")]
    Block(String),
    #[error("trailing bytes after finalized consensus block")]
    TrailingBlock,
    #[error("finalization committee decode bound must be non-zero")]
    ZeroCommitteeBound,
}

impl PublicFinalizedBlockDecodeError {
    fn finalization(error: impl fmt::Display) -> Self {
        Self::Finalization(error.to_string())
    }

    fn block(error: impl fmt::Display) -> Self {
        Self::Block(error.to_string())
    }
}

/// Canonically decode the two byte strings exposed by
/// `outbe_getFinalization(height)`.
///
/// Decoding establishes structure only. Callers must still authenticate the
/// certificate against the historical committee before treating it as
/// finality authority.
pub fn decode_public_finalized_block(
    finalization_bytes: &[u8],
    block_bytes: &[u8],
    max_committee_members: usize,
) -> Result<CertifiedFinalizedBlock, PublicFinalizedBlockDecodeError> {
    if max_committee_members == 0 {
        return Err(PublicFinalizedBlockDecodeError::ZeroCommitteeBound);
    }
    let finalization = decode_public_finalization(finalization_bytes, max_committee_members)?;

    let block = decode_public_block(block_bytes)?;
    Ok(CertifiedFinalizedBlock {
        finalization,
        block,
    })
}

pub fn decode_public_block(
    block_bytes: &[u8],
) -> Result<ConsensusBlock, PublicFinalizedBlockDecodeError> {
    let mut block_reader = block_bytes;
    let block = ConsensusBlock::read_cfg(&mut block_reader, &())
        .map_err(PublicFinalizedBlockDecodeError::block)?;
    if !block_reader.is_empty() {
        return Err(PublicFinalizedBlockDecodeError::TrailingBlock);
    }
    Ok(block)
}

/// Canonically decode one public Commonware finalization without trusting it.
/// The caller must verify the returned certificate against the exact historical
/// committee and bind its proposal payload to the authenticated header hash.
pub fn decode_public_finalization(
    finalization_bytes: &[u8],
    max_committee_members: usize,
) -> Result<Finalization, PublicFinalizedBlockDecodeError> {
    if max_committee_members == 0 {
        return Err(PublicFinalizedBlockDecodeError::ZeroCommitteeBound);
    }
    let mut reader = finalization_bytes;
    let finalization = Finalization::read_cfg(&mut reader, &max_committee_members)
        .map_err(PublicFinalizedBlockDecodeError::finalization)?;
    if !reader.is_empty() {
        return Err(PublicFinalizedBlockDecodeError::TrailingFinalization);
    }
    Ok(finalization)
}
