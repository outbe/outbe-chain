//! Canonical entity payload conversion, independent of the storage backend.

use crate::{NodBucketState, NodItemState};
use outbe_primitives::{
    entity_codec::{NodBucketBodyV1, NodItemBodyV2},
    wwd_entity_id::WwdEntityId,
};

/// Converts one runtime Nod item into its normative encrypted v2 payload model.
pub fn canonical_item(body: &NodItemState) -> NodItemBodyV2 {
    NodItemBodyV2 {
        encrypted: body.encrypted.clone(),
        bucket_key: body.bucket_key,
        issued_at: body.issued_at,
        is_settled: body.is_settled,
    }
}

/// Converts one runtime Nod bucket into its normative v1 payload model.
pub fn canonical_bucket(body: &NodBucketState) -> NodBucketBodyV1 {
    NodBucketBodyV1 {
        settled_nods: body.settled_nods,
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
    }
}

/// Reconstructs the canonical bucket identity from the body.
pub fn canonical_bucket_id(body: &NodBucketState) -> WwdEntityId {
    canonical_bucket(body).entity_id()
}

/// Converts a validated normative encrypted v2 payload into the runtime item type.
pub fn from_canonical_item(body: NodItemBodyV2) -> NodItemState {
    let terms = &body.encrypted.terms;
    NodItemState {
        nod_id: terms.nod_id,
        owner: terms.owner,
        worldwide_day: terms.worldwide_day,
        league_id: terms.league_id,
        issuance_currency: terms.issuance_currency,
        reference_currency: terms.reference_currency,
        encrypted: body.encrypted,
        bucket_key: body.bucket_key,
        issued_at: body.issued_at,
        is_settled: body.is_settled,
    }
}

/// Converts a validated normative v1 payload into the runtime bucket type.
pub fn from_canonical_bucket(body: NodBucketBodyV1) -> NodBucketState {
    NodBucketState {
        settled_nods: body.settled_nods,
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
    }
}
