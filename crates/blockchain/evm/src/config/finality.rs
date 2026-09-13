use outbe_metadosis::api::{OcompFinalityAuthorityError, OcompFinalizedIntentAuthority};

use reth_provider::{BlockHashReader, BlockIdReader};
use std::sync::Arc;

/// Live execution wrapper that first binds the relayer-carried request header
/// to this node's canonical finalized chain, then delegates cryptographic and
/// storage-proof verification to the protocol authority.
pub(super) struct ProviderAnchoredOcompFinalityAuthority<P> {
    provider: P,
    inner: Arc<dyn OcompFinalizedIntentAuthority>,
}

impl<P> ProviderAnchoredOcompFinalityAuthority<P> {
    pub(super) fn new(provider: P, inner: Arc<dyn OcompFinalizedIntentAuthority>) -> Self {
        Self { provider, inner }
    }
}

impl<P> OcompFinalizedIntentAuthority for ProviderAnchoredOcompFinalityAuthority<P>
where
    P: BlockHashReader + BlockIdReader + Send + Sync,
{
    fn verify(
        &self,
        proof: &outbe_ocomp_protocol::intent::FinalizedIntentProofV1,
        expected: outbe_ocomp_protocol::intent::ExpectedFinalizedIntentBindingV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> Result<outbe_ocomp_protocol::intent::VerifiedFinalizedIntentV1, OcompFinalityAuthorityError>
    {
        let claimed = &proof.parent_accounting;
        let finalized = self
            .provider
            .finalized_block_num_hash()
            .map_err(|error| OcompFinalityAuthorityError::LocalAuthority(error.to_string()))?
            .ok_or_else(|| {
                OcompFinalityAuthorityError::LocalAuthority(
                    "finalized checkpoint is unavailable".into(),
                )
            })?;
        if claimed.finalized_block_number > finalized.number {
            return Err(OcompFinalityAuthorityError::LocalAuthority(format!(
                "claimed request height {} is above local finalized height {}",
                claimed.finalized_block_number, finalized.number
            )));
        }
        let canonical_hash = self
            .provider
            .block_hash(claimed.finalized_block_number)
            .map_err(|error| OcompFinalityAuthorityError::LocalAuthority(error.to_string()))?
            .ok_or_else(|| {
                OcompFinalityAuthorityError::LocalAuthority(format!(
                    "canonical hash at finalized height {} is unavailable",
                    claimed.finalized_block_number
                ))
            })?;
        if canonical_hash != claimed.finalized_block_hash {
            return Err(OcompFinalityAuthorityError::InvalidProof(
                outbe_ocomp_protocol::intent::FinalizedIntentVerificationError::FinalizedHeaderMetadataMismatch,
            ));
        }
        self.inner.verify(proof, expected, limits)
    }
}
