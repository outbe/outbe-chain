use super::*;

/// Enforce the permanent offer-key invariant before any threshold ceremony,
/// live join, reshare, or consensus actor can start.
///
/// Only proven block-1 founders may be keyless while canonical state is still
/// zero. An empty-DB verifier join may carry an already-installed key until
/// certified sync makes the canonical value locally available. Every other
/// existing identity must match canonical state exactly at this gate.
pub(in crate::stack) fn validate_offer_key_before_threshold_work(
    context: StartupDkgContext,
    local_key_in_current_consensus_set: bool,
    verifier_join: bool,
    on_chain_offer: B256,
    resident_offer: Option<B256>,
) -> Result<()> {
    let founding = !verifier_join
        && startup_dkg_mode(context, local_key_in_current_consensus_set)
            == StartupDkgMode::InitialGenesisDkg;
    if founding {
        if on_chain_offer.is_zero() {
            Ok(())
        } else {
            Err(crate::errors::StackDkgError::FreshGenesisFoundPreexistingOfferKey.into())
        }
    } else {
        validate_existing_offer_key(context, verifier_join, on_chain_offer, resident_offer)
    }
}

fn validate_existing_offer_key(
    context: StartupDkgContext,
    verifier_join: bool,
    on_chain_offer: B256,
    resident_offer: Option<B256>,
) -> Result<()> {
    let resident_offer = resident_offer
        .ok_or(crate::errors::StackDkgError::ExistingIdentityMissingResidentOfferKey)?;
    if resident_offer.is_zero() {
        return Err(crate::errors::StackDkgError::ExistingIdentityZeroResidentOfferKey.into());
    }

    let has_existing_local_state = context.last_execution_height > 0
        || context.last_consensus_finalized_height > 0
        || context.has_chain_finalized_dkg_boundary();
    if on_chain_offer.is_zero() {
        if has_existing_local_state {
            return Err(crate::errors::StackDkgError::ExistingCanonicalStateMissingOfferKey.into());
        }
        if !verifier_join {
            return Err(crate::errors::StackDkgError::NonVerifierDeferredOfferKeyComparison.into());
        }
        Ok(())
    } else if resident_offer != on_chain_offer {
        Err(crate::errors::StackDkgError::ResidentOfferKeyMismatch.into())
    } else {
        Ok(())
    }
}
