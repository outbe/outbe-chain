//! Static issuance invariants checked before Nod storage is changed.
use crate::{
    errors::{date_step_error, NodError},
    NodItemState,
};
use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::nod_rules::derive_nod_id;
pub(crate) fn validate_item(
    item: &NodItemState,
    call_price_minor: U256,
    expected_group_key: Option<alloy_primitives::B256>,
) -> Result<()> {
    // ISO 0 is not a currency, and its bin namespace aliases the
    // un-namespaced key while never appearing in the oracle's
    // reference-currency registry — a group parked there would be
    // invisible to the call scan forever.
    if item.reference_currency == 0 {
        return Err(NodError::ZeroReferenceCurrency.into());
    }
    let canonical_id = derive_nod_id(item.series_id, item.owner);
    if item.nod_id != canonical_id {
        return Err(NodError::CanonicalIdentityMismatch {
            expected: canonical_id,
            found: item.nod_id,
        }
        .into());
    }
    let terms = &item.encrypted.terms;
    if !item.encrypted.has_valid_encoding()
        || terms.series_id().ok() != Some(item.series_id)
        || (
            terms.nod_id,
            terms.owner,
            terms.league_id,
            terms.issuance_currency,
            terms.reference_currency,
        ) != (
            item.nod_id,
            item.owner,
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

    let canonical_group_key = match expected_group_key {
        Some(expected) => expected,
        None => {
            crate::identity::group_key(item.issued_at, call_price_minor, item.reference_currency)
                .map_err(date_step_error)?
        }
    };
    if item.group_key != canonical_group_key {
        return Err(NodError::GroupIdentityMismatch {
            expected: canonical_group_key,
            found: item.group_key,
        }
        .into());
    }
    Ok(())
}
