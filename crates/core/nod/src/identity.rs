//! Pure NOD identities shared by issuance and offchain calculation.
use crate::errors::NodError;
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, WwdEntityId};
use outbe_primitives::storage::types::StorageKey;
use outbe_primitives::{error::Result, time::WorldwideDay};

/// Computes the bucket key from
/// `(worldwide_day, entry_price_minor, reference_currency)`.
///
/// The currency is part of the preimage because `entry_price_minor` is
/// denominated in it. Two Nods that share a day and an entry value in
/// different currencies are priced against different oracle rates. They must
/// not share a bucket. This is the single derivation. The Lysis program
/// calls it too, so the off-chain and on-chain keys cannot drift.
pub fn bucket_key(
    worldwide_day: WorldwideDay,
    entry_price_minor: U256,
    reference_currency: u16,
) -> B256 {
    use alloy_primitives::keccak256;
    let mut buf = [0u8; 38];
    buf[0..4].copy_from_slice(worldwide_day.key_bytes().as_slice());
    buf[4..36].copy_from_slice(&entry_price_minor.to_be_bytes::<32>());
    buf[36..38].copy_from_slice(&reference_currency.to_be_bytes());
    keccak256(buf)
}

/// Deterministic full-width Poseidon NOD identity derived from
/// `(owner, worldwide_day)`. The typed Nod collection is its namespace.
pub fn generate_nod_id(
    owner: Address,
    worldwide_day: WorldwideDay,
) -> outbe_primitives::error::Result<WwdEntityId> {
    derive_poseidon_entity_id(owner, worldwide_day)
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))
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
