use outbe_metadosis::api::{OcompFinalityAuthorityError, OcompFinalizedIntentAuthority};

use reth_provider::{BlockHashReader, BlockIdReader};
use std::sync::Arc;

/// Live execution wrapper. It first binds the relayer-carried request header
/// to this node's canonical finalized chain. It then delegates cryptographic and
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
            .map_err(crate::errors::FinalityAuthorityLocalError::provider)?
            .ok_or(crate::errors::FinalityAuthorityLocalError::FinalizedCheckpointUnavailable)?;
        if claimed.finalized_block_number > finalized.number {
            return Err(
                crate::errors::FinalityAuthorityLocalError::ClaimedHeightAboveFinalized {
                    claimed: claimed.finalized_block_number,
                    finalized: finalized.number,
                }
                .into(),
            );
        }
        let canonical_hash = self
            .provider
            .block_hash(claimed.finalized_block_number)
            .map_err(crate::errors::FinalityAuthorityLocalError::provider)?
            .ok_or(
                crate::errors::FinalityAuthorityLocalError::CanonicalHashUnavailable {
                    height: claimed.finalized_block_number,
                },
            )?;
        if canonical_hash != claimed.finalized_block_hash {
            return Err(OcompFinalityAuthorityError::InvalidProof(
                outbe_ocomp_protocol::intent::FinalizedIntentVerificationError::FinalizedHeaderMetadataMismatch,
            ));
        }
        self.inner.verify(proof, expected, limits)
    }
}
