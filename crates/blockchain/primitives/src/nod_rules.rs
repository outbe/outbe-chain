//! Pure rules shared by Lysis and Nod issuance.

use alloy_primitives::{Address, U256};

/// `keccak256(series_id || owner)`: one Nod per owner and series.
pub fn derive_nod_id(series_id: crate::series_id::SeriesId, owner: Address) -> U256 {
    series_id.right_id(&[owner.as_slice()])
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
    fn nod_matches_independent_packed_keccak_vector_and_binds_series_and_owner() {
        let owner = Address::repeat_byte(0x42);
        let series = crate::series_id::SeriesId::for_pair(20261029, 978, 840).unwrap();
        let preimage = alloy_primitives::hex!(
            "32303236313032392d4555522d554242424242424242424242424242424242424242"
        );
        assert_eq!(&preimage[..14], series.as_bytes());
        assert_eq!(&preimage[14..], owner.as_slice());
        // Independently computed with `cast keccak` over the literal packed bytes.
        let expected = U256::from_str_radix(
            "202f86f0415c7790e21a3c2ea9dfdbd056ae1fca6ca12393ea84980b32fe73d6",
            16,
        )
        .unwrap();
        assert_eq!(derive_nod_id(series, owner), expected);
        assert_ne!(derive_nod_id(series, Address::repeat_byte(0x43)), expected);
        assert_ne!(
            derive_nod_id(
                crate::series_id::SeriesId::for_pair(20261030, 978, 840).unwrap(),
                owner
            ),
            expected
        );
    }
}
