//! Canonical entity payload conversion, independent of the storage backend.

use crate::{NodGroupState, NodItemState};
use outbe_primitives::entity_codec::{NodGroupBodyV1, NodItemBodyV1};

/// Converts one runtime Nod item into its normative encrypted v1 payload model.
pub fn canonical_item(body: &NodItemState) -> NodItemBodyV1 {
    NodItemBodyV1 {
        encrypted: body.encrypted.clone(),
        group_key: body.group_key,
        issued_at: body.issued_at,
        is_settled: body.is_settled,
    }
}

/// Converts one runtime Nod group into its normative v1 payload model.
pub fn canonical_group(body: &NodGroupState) -> NodGroupBodyV1 {
    NodGroupBodyV1 {
        settled_nods: body.settled_nods,
        group_key: body.group_key,
        full_day_after_issue: body.full_day_after_issue,
        reference_currency: body.reference_currency,
    }
}

/// Converts a validated normative encrypted v1 payload into the runtime item type.
pub fn from_canonical_item(body: NodItemBodyV1) -> NodItemState {
    let terms = &body.encrypted.terms;
    NodItemState {
        nod_id: terms.nod_id,
        owner: terms.owner,
        series_id: terms.series_id().unwrap_or_default(),
        league_id: terms.league_id,
        issuance_currency: terms.issuance_currency,
        reference_currency: terms.reference_currency,
        encrypted: body.encrypted,
        group_key: body.group_key,
        issued_at: body.issued_at,
        is_settled: body.is_settled,
    }
}

/// Converts a validated normative v1 payload into the runtime group type.
pub fn from_canonical_group(body: NodGroupBodyV1) -> NodGroupState {
    NodGroupState {
        settled_nods: body.settled_nods,
        group_key: body.group_key,
        full_day_after_issue: body.full_day_after_issue,
        reference_currency: body.reference_currency,
    }
}
