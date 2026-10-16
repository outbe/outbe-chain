//! Canonical entity payload conversion, independent of the storage backend.

use crate::identity::bucket_id;
use crate::{GemBucketState, GemItemState};
use outbe_primitives::{
    entity_codec::{GemBodyV1, GemBucketBodyV1},
    wwd_entity_id::WwdEntityId,
};

/// A canonical payload field does not fit its runtime type.
#[derive(Debug, thiserror::Error)]
pub enum GemCodecError {
    #[error("canonical gem field {field} is outside the runtime range")]
    CanonicalFieldOutOfRange { field: &'static str },
}

fn narrow_u8<T>(value: T, field: &'static str) -> Result<u8, GemCodecError>
where
    T: TryInto<u8>,
{
    value
        .try_into()
        .map_err(|_| GemCodecError::CanonicalFieldOutOfRange { field })
}

/// Converts one runtime gem item into its normative v1 payload model.
pub fn canonical_item(body: &GemItemState) -> GemBodyV1 {
    GemBodyV1 {
        gem_id: body.gem_id,
        owner: body.owner,
        gem_type: u32::from(body.gem_type),
        promis_load_minor: body.promis_load_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
        bucket_key: body.bucket_key,
        state: u32::from(body.state),
        called_at: body.called_at,
        settled_at: body.settled_at,
    }
}

/// Converts one runtime gem bucket into its normative v1 payload model.
pub fn canonical_bucket(body: &GemBucketState) -> GemBucketBodyV1 {
    GemBucketBodyV1 {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
        settled_gems: body.settled_gems,
    }
}

/// Reconstructs the canonical bucket identity from the body.
#[must_use]
pub fn canonical_bucket_id(body: &GemBucketState) -> WwdEntityId {
    bucket_id(body.worldwide_day, body.bucket_key)
}

/// Converts a validated normative v1 payload into the runtime item type.
pub fn from_canonical_item(body: GemBodyV1) -> Result<GemItemState, GemCodecError> {
    Ok(GemItemState {
        gem_id: body.gem_id,
        owner: body.owner,
        gem_type: narrow_u8(body.gem_type, "gem_type")?,
        promis_load_minor: body.promis_load_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
        bucket_key: body.bucket_key,
        state: narrow_u8(body.state, "state")?,
        called_at: body.called_at,
        settled_at: body.settled_at,
    })
}

/// Converts a validated normative v1 payload into the runtime bucket type.
pub fn from_canonical_bucket(body: GemBucketBodyV1) -> Result<GemBucketState, GemCodecError> {
    Ok(GemBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
        settled_gems: body.settled_gems,
    })
}
