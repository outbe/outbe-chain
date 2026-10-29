//! Pure rules shared by Lysis and Nod issuance.

use alloy_primitives::{keccak256, Address, U256};

/// Full Keccak256 digest interpreted as a big-endian U256.
/// The exact 24-byte preimage is `owner` (raw 20 bytes) followed by
/// `worldwide_day` (u32 big-endian), without padding, prefix, or separator.
/// The issuance day is stored separately.
pub fn derive_nod_id(owner: Address, worldwide_day: crate::time::WorldwideDay) -> U256 {
    let mut preimage = [0u8; 24];
    preimage[..20].copy_from_slice(owner.as_slice());
    preimage[20..].copy_from_slice(&worldwide_day.value().to_be_bytes());
    U256::from_be_bytes(keccak256(preimage).0)
}

use crate::{
    error::Result,
    settlement::{settlement_cost_units, SettlementCost},
    units::PROTOCOL_AMOUNT_DECIMALS,
};

pub fn is_issuable_entry(entry: U256) -> bool {
    crate::markup::marked_up(entry, u16::MAX).is_some()
}

/// One Nod's cost in reference-currency minor units, by the settlement cost rule.
pub fn settlement_cost_minor(entry: U256, load: U256) -> Result<U256> {
    let cost = SettlementCost {
        entry_price_minor: entry,
        load_minor: load,
        units: U256::ONE,
    };
    Ok(settlement_cost_units(
        &cost,
        None,
        PROTOCOL_AMOUNT_DECIMALS,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nod_matches_independent_packed_keccak_vector_and_binds_owner_and_day() {
        let owner = Address::repeat_byte(0x42);
        let day = crate::time::WorldwideDay::new(0x01020304);
        let preimage = alloy_primitives::hex!("424242424242424242424242424242424242424201020304");
        assert_eq!(&preimage[..20], owner.as_slice());
        assert_eq!(&preimage[20..], &day.value().to_be_bytes());
        // Independently computed with ethers.keccak256 over the literal packed bytes.
        let expected = U256::from_str_radix(
            "501ba769d4ff8f2faa11545b1608051d3009bb0cdd2518ef005b88ec71b5243b",
            16,
        )
        .unwrap();
        assert_eq!(U256::from_be_bytes(keccak256(preimage).0), expected);
        assert_eq!(derive_nod_id(owner, day), expected);
        assert_ne!(derive_nod_id(Address::repeat_byte(0x43), day), expected);
        assert_ne!(
            derive_nod_id(owner, crate::time::WorldwideDay::new(0x01020305)),
            expected
        );
        assert_ne!(
            derive_nod_id(owner, day),
            crate::wwd_entity_id::derive_poseidon_entity_id(owner, day).to_u256()
        );
    }
}
