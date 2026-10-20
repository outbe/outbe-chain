//! Static issuance invariants checked before Nod storage is changed.
use crate::{errors::NodError, NodItemState};
use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::wwd_entity_id::derive_poseidon_entity_id;
pub(crate) fn validate_item(item: &NodItemState, call_price_minor: U256) -> Result<()> {
    let canonical_id = derive_poseidon_entity_id(item.owner, item.worldwide_day)
        .map_err(NodError::entity_id_derivation)?;
    if item.nod_id != canonical_id {
        return Err(NodError::CanonicalIdentityMismatch {
            expected: canonical_id,
            found: item.nod_id,
        }
        .into());
    }
    let terms = &item.encrypted.terms;
    if !item.encrypted.has_valid_encoding()
        || (
            terms.nod_id,
            terms.owner,
            terms.worldwide_day,
            terms.league_id,
            terms.issuance_currency,
            terms.reference_currency,
        ) != (
            item.nod_id,
            item.owner,
            item.worldwide_day,
            item.league_id,
            item.issuance_currency,
            item.reference_currency,
        )
    {
        return Err(NodError::EncryptedTermsMismatch.into());
    }
    if item.is_settled {
        return Err(NodError::CannotIssueSettledNod.into());
    }
    // ISO 0 is not a currency, and its bin namespace aliases the
    // un-namespaced key while never appearing in the oracle's
    // reference-currency registry — a group parked there would be
    // invisible to the call scan forever.
    if item.reference_currency == 0 {
        return Err(NodError::ZeroReferenceCurrency.into());
    }

    let canonical_group_key =
        crate::identity::group_key(item.issued_at, call_price_minor, item.reference_currency);
    if item.group_key != canonical_group_key {
        return Err(NodError::GroupIdentityMismatch {
            expected: canonical_group_key,
            found: item.group_key,
        }
        .into());
    }
    Ok(())
}
