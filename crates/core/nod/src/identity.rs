//! Pure NOD identities shared by issuance and offchain calculation.
use crate::errors::NodError;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::Result,
    time::{TimeError, WorldwideDay},
};

/// The call group of a Nod issued at `issued_at`: its first full day, its call price and
/// the currency that price is quoted in.
pub fn group_key(
    issued_at: u64,
    call_price_minor: U256,
    reference_currency: u16,
) -> std::result::Result<B256, TimeError> {
    Ok(outbe_primitives::call_breach::group_key(
        outbe_primitives::time::first_full_day(issued_at)?,
        call_price_minor,
        reference_currency,
    ))
}

/// Series group identity sealed at Lysis: `(emission_day E, issuance_currency, reference_currency)` (OIP-00343 / OIP-00328).
#[must_use]
pub fn series_group_key(
    emission_day: u32,
    issuance_currency: u16,
    reference_currency: u16,
) -> B256 {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&emission_day.to_be_bytes());
    bytes[4..6].copy_from_slice(&issuance_currency.to_be_bytes());
    bytes[6..8].copy_from_slice(&reference_currency.to_be_bytes());
    alloy_primitives::keccak256(bytes)
}

/// Deterministic full-width Poseidon2 NOD identity derived from
/// `(owner, worldwide_day)`. The typed Nod collection is its namespace.
pub fn generate_nod_id(owner: Address, worldwide_day: WorldwideDay) -> U256 {
    outbe_primitives::nod_rules::derive_nod_id(owner, worldwide_day)
}

pub fn parse_nod_id(nod_id: &str) -> Result<U256> {
    let trimmed = nod_id.strip_prefix("0x").unwrap_or(nod_id);
    if trimmed.len() != 64 {
        return Err(NodError::InvalidNodIdLength.into());
    }
    if !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(NodError::InvalidNodIdHex.into());
    }
    U256::from_str_radix(trimmed, 16).map_err(|_| NodError::InvalidNodIdHex.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_full_width_hex_and_preserves_leading_zeroes() {
        for id in [U256::ONE, U256::from(0x1234), U256::MAX] {
            let hex = format!("{id:064x}");
            assert_eq!(parse_nod_id(&hex).unwrap(), id);
            assert_eq!(parse_nod_id(&format!("0x{hex}")).unwrap(), id);
            assert!(parse_nod_id(&hex[1..]).is_err());
            assert!(parse_nod_id(&format!("{hex}0")).is_err());
        }
        assert_eq!(
            parse_nod_id(&format!("{:0>64}", "10")).unwrap(),
            U256::from(16)
        );
        assert!(parse_nod_id(&"g".repeat(64)).is_err());
        assert!(parse_nod_id(&format!("{}_{:0>31}", "0".repeat(32), "1")).is_err());
        assert!(parse_nod_id(&format!("+{:0>63}", "1")).is_err());
    }
}
