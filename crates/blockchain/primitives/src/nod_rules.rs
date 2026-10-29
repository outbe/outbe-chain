//! Pure rules shared by Lysis and Nod issuance.

use alloy_primitives::{Address, U256};

/// Opaque full Poseidon2 digest. The issuance day is stored separately.
pub fn derive_nod_id(owner: Address, worldwide_day: crate::time::WorldwideDay) -> U256 {
    U256::from_be_bytes(crate::wwd_entity_id::derive_poseidon_digest(owner, worldwide_day).0)
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
    fn nod_retains_the_full_independent_noir_digest() {
        let owner = Address::repeat_byte(0x42);
        let day = crate::time::WorldwideDay::new(0x01020304);
        let expected = U256::from_str_radix(
            "14edf11805beb34363ddabb6c010e0b5883bfe90d9e06ff2eeffa66f61683c0b",
            16,
        )
        .unwrap();
        assert_eq!(derive_nod_id(owner, day), expected);
        assert_ne!(
            derive_nod_id(owner, day),
            crate::wwd_entity_id::derive_poseidon_entity_id(owner, day).to_u256()
        );
    }
}
