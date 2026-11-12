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
    use outbe_primitives::{
        consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
        system_tx::{SystemTx, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX},
    };
    if parent.number().checked_add(1)? != child.number() || child.parent_digest() != parent.digest()
    {
        return None;
    }
    for transaction in &child.body().transactions {
        let Ok(SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload,
        }) = SystemTx::decode(transaction.input().as_ref())
        else {
            continue;
        };
        let Ok(metadata) = CertifiedParentAccountingMetadata::decode(payload.as_ref()) else {
            continue;
        };
        let Ok(reencoded) = metadata.encode() else {
            continue;
        };
        if reencoded.as_ref() != payload.as_ref() {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::SignableTransaction as _;
    use alloy_primitives::{Bytes, Signature, B256};
    use commonware_codec::Encode as _;
    use commonware_consensus::types::Epoch;
    use outbe_primitives::{
        consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
        system_tx::{
            build_unsigned_system_tx, SystemTx, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
        },
        OutbeBlock,
    };
    use reth_primitives_traits::SealedBlock;

    fn child_with_payload(
        parent: &ConsensusBlock,
        step_index: u8,
        payload: Bytes,
    ) -> ConsensusBlock {
        let calldata = SystemTx::Consensus {
            step_index,
            payload,
        }
        .encode()
        .expect("encode");
        let transaction = build_unsigned_system_tx(0, parent.number() + 1, 1, calldata)
            .expect("system transaction")
            .into_signed(Signature::test_signature())
            .into();
        let mut block = OutbeBlock::default();
        block.header.inner.number = parent.number() + 1;
        block.header.inner.parent_hash = parent.block_hash();
        block.body.transactions.push(transaction);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    #[test]
    fn parent_finalization_requires_canonical_accounting_payload() {
        let fixture = crate::finalized_admission_test_utils::FinalityCommitteeFixture::new(30)
            .certify_block(Epoch::new(2), 10, 100, B256::ZERO, Vec::new());
        let parent = decode_public_block(&fixture.block).expect("parent block");
        let metadata = CertifiedParentAccountingMetadata {
            finalized_block_number: parent.number(),
            finalized_block_hash: parent.block_hash(),
            finalized_epoch: 2,
            proof: fixture.finalization.into(),
            ..Default::default()
        };
        let payload = metadata.encode().expect("metadata");
        let child = child_with_payload(
            &parent,
            CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload.clone(),
        );
        let extracted =
            parent_finalization_from_child(&parent, &child).expect("parent finalization");
        assert_eq!(extracted.encode().as_ref(), metadata.proof.as_ref());

        let mut trailing = payload.to_vec();
        trailing.push(0);
        let child = child_with_payload(
            &parent,
            CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            trailing.into(),
        );
        assert!(parent_finalization_from_child(&parent, &child).is_none());

        let child = child_with_payload(&parent, CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX, payload);
        assert!(parent_finalization_from_child(&parent, &child).is_none());
        for invalid in [
            CertifiedParentAccountingMetadata {
                proof_kind: ParentParticipationProof::CertifiedNotarization,
                ..metadata.clone()
            },
            CertifiedParentAccountingMetadata {
                finalized_block_number: 9,
                ..metadata.clone()
            },
            CertifiedParentAccountingMetadata {
                finalized_block_hash: B256::ZERO,
                ..metadata.clone()
            },
            CertifiedParentAccountingMetadata {
                finalized_epoch: 3,
                ..metadata
            },
        ] {
            let child = child_with_payload(
                &parent,
                CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                invalid.encode().expect("metadata"),
            );
            assert!(parent_finalization_from_child(&parent, &child).is_none());
        }
    }
}
