//! Canonical entity payload conversion, independent of the storage backend.

use crate::TributeData;
use outbe_primitives::entity_codec::TributeBodyV1;

/// Converts the runtime body into its normative v1 payload model.
pub fn canonical_body(body: &TributeData) -> TributeBodyV1 {
    TributeBodyV1 {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_amount_minor: body.issuance_amount_minor,
        issuance_currency: body.issuance_currency,
        nominal_amount_minor: body.nominal_amount_minor,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
    }
}

/// Converts a validated normative v1 payload into the runtime body type.
pub fn from_canonical_body(body: TributeBodyV1) -> TributeData {
    TributeData {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_amount_minor: body.issuance_amount_minor,
        issuance_currency: body.issuance_currency,
        nominal_amount_minor: body.nominal_amount_minor,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
    }
}
