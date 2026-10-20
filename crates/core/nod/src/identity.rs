//! Pure NOD identities shared by issuance and offchain calculation.
use crate::errors::NodError;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId};
use outbe_primitives::{error::Result, time::WorldwideDay};

/// The call group of a Nod issued at `issued_at`: its first full day, its call price and
/// the currency that price is quoted in.
pub fn group_key(issued_at: u64, call_price_minor: U256, reference_currency: u16) -> B256 {
    outbe_primitives::call_breach::group_key(
        outbe_primitives::time::first_full_day(issued_at),
        call_price_minor,
        reference_currency,
    )
}

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
