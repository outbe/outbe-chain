//! Pure NOD identities shared by issuance and offchain calculation.
use crate::errors::NodError;
use alloy_primitives::Address;
use outbe_primitives::wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId};
use outbe_primitives::{error::Result, time::WorldwideDay};

/// Computes the bucket key from
/// `(worldwide_day, entry_price_minor, reference_currency)`.
///
/// The currency is part of the preimage because `entry_price_minor` is
/// denominated in it. Two Nods that share a day and an entry value in
/// different currencies are priced against different oracle rates. They must
/// not share a bucket. This is the single derivation. The Lysis program
/// calls it too, so the off-chain and on-chain keys cannot drift.
pub use outbe_primitives::nod_rules::bucket_key;

/// Deterministic full-width Poseidon2 NOD identity derived from
/// `(owner, worldwide_day)`. The typed Nod collection is its namespace.
pub fn generate_nod_id(
    owner: Address,
    worldwide_day: WorldwideDay,
) -> outbe_primitives::error::Result<WwdEntityId> {
    derive_poseidon_entity_id(owner, worldwide_day).map_err(NodError::entity_id_derivation)
}

pub fn parse_nod_id(nod_id: &str) -> Result<WwdEntityId> {
    let trimmed = nod_id.strip_prefix("0x").unwrap_or(nod_id);
    if trimmed.len() != WwdEntityId::len_bytes() * 2 {
        return Err(NodError::InvalidNodIdLength.into());
    }
    trimmed
        .parse::<WwdEntityId>()
        .map_err(|_| NodError::InvalidNodIdHex.into())
}
