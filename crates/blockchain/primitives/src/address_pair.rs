use alloy_primitives::{keccak256, wrap_fixed_bytes, Address, U256};
// `wrap_fixed_bytes!` expands to `derive_more` derives that emit unqualified
// paths, so the crate has to be nameable here.
use alloy_primitives::private::derive_more;

use crate::storage::types::StorageKey;

wrap_fixed_bytes!(
    /// Two Ethereum addresses concatenated, 40 bytes in length.
    ///
    /// Declared with the [`wrap_fixed_bytes!`] macro like [`Address`] itself,
    /// so it inherits hex parsing, formatting, and `FixedBytes` conversions.
    pub struct AddressPair<40>;
);

impl AddressPair {
    /// Packs both addresses in ascending order, so the same two addresses
    /// yield one key whichever side the caller quotes first. The lower address
    /// occupies bytes `0..20` and the higher one bytes `20..40`.
    ///
    /// `new` is already taken by the raw `[u8; 40]` constructor that
    /// [`wrap_fixed_bytes!`] generates.
    pub fn from_addresses(first: Address, second: Address) -> Self {
        let (low, high) = if first <= second {
            (first, second)
        } else {
            (second, first)
        };
        let mut bytes = [0u8; 40];
        bytes[0..20].copy_from_slice(low.as_slice());
        bytes[20..40].copy_from_slice(high.as_slice());
        Self::from(bytes)
    }

    /// The lower of the two packed addresses.
    pub fn first(&self) -> Address {
        Address::from_slice(&self[0..20])
    }

    /// The higher of the two packed addresses.
    pub fn second(&self) -> Address {
        Address::from_slice(&self[20..40])
    }

    /// Both packed addresses in ascending order.
    pub fn pair(&self) -> (Address, Address) {
        (self.first(), self.second())
    }
}

/// Usable as a `Mapping` key, but never as a value: [`Storable`] round-trips
/// through a single 32-byte word and 40 bytes do not fit one.
///
/// [`Storable`]: crate::storage::types::Storable
impl StorageKey for AddressPair {
    fn key_bytes(&self) -> Vec<u8> {
        self.as_slice().to_vec()
    }

    /// Solidity left-pads a mapping key only when it is narrower than a word;
    /// a wider key is concatenated with the base slot as-is. The provided
    /// implementation computes `32 - key.len()`, which a 40-byte key underflows,
    /// so the concatenation is spelled out here against a fixed-size buffer.
    fn mapping_slot(&self, base_slot: U256) -> U256 {
        let mut buf = [0u8; 72];
        buf[..40].copy_from_slice(self.as_slice());
        buf[40..].copy_from_slice(&base_slot.to_be_bytes::<32>());
        U256::from_be_bytes(keccak256(buf).0)
    }
}

#[cfg(test)]
mod tests {
    use super::AddressPair;
    use crate::storage::types::StorageKey;
    use alloy_primitives::{address, b256, keccak256, Address, U256};

    const FIRST: Address = address!("0x1111111111111111111111111111111111111111");
    const SECOND: Address = address!("0x2222222222222222222222222222222222222222");
    const THIRD: Address = address!("0x3333333333333333333333333333333333333333");

    #[test]
    fn from_addresses_packs_both_addresses_in_ascending_order() {
        let pair = AddressPair::from_addresses(FIRST, SECOND);

        assert_eq!(AddressPair::len_bytes(), 40);
        assert_eq!(&pair[0..20], FIRST.as_slice());
        assert_eq!(&pair[20..40], SECOND.as_slice());
    }

    #[test]
    fn the_accessors_return_the_packed_addresses_in_ascending_order() {
        let pair = AddressPair::from_addresses(SECOND, FIRST);

        assert_eq!(pair.first(), FIRST);
        assert_eq!(pair.second(), SECOND);
        assert_eq!(pair.pair(), (FIRST, SECOND));
    }

    #[test]
    fn the_accessors_round_trip_back_into_the_same_pair() {
        let pair = AddressPair::from_addresses(FIRST, SECOND);

        assert_eq!(
            AddressPair::from_addresses(pair.first(), pair.second()),
            pair
        );
    }

    #[test]
    fn from_addresses_ignores_the_argument_order() {
        assert_eq!(
            AddressPair::from_addresses(FIRST, SECOND),
            AddressPair::from_addresses(SECOND, FIRST),
        );
    }

    #[test]
    fn from_addresses_separates_pairs_sharing_an_address() {
        assert_ne!(
            AddressPair::from_addresses(FIRST, SECOND),
            AddressPair::from_addresses(FIRST, THIRD),
        );
    }

    #[test]
    fn from_addresses_packs_an_address_paired_with_itself() {
        let pair = AddressPair::from_addresses(FIRST, FIRST);

        assert_eq!(&pair[0..20], FIRST.as_slice());
        assert_eq!(&pair[20..40], FIRST.as_slice());
    }

    #[test]
    fn a_pair_round_trips_through_hex() {
        let pair = AddressPair::from_addresses(FIRST, SECOND);

        assert_eq!(pair.to_string().parse::<AddressPair>(), Ok(pair));
    }

    #[test]
    fn the_zero_pair_holds_forty_zero_bytes() {
        assert_eq!(
            AddressPair::from_addresses(Address::ZERO, Address::ZERO),
            AddressPair::ZERO,
        );
    }

    #[test]
    fn the_mapping_slot_concatenates_the_forty_byte_key_with_the_base_slot() {
        let pair = AddressPair::from_addresses(FIRST, SECOND);
        let base_slot = U256::from(10u64);

        let mut expected = Vec::with_capacity(72);
        expected.extend_from_slice(pair.as_slice());
        expected.extend_from_slice(&base_slot.to_be_bytes::<32>());

        assert_eq!(
            pair.mapping_slot(base_slot),
            U256::from_be_bytes(keccak256(&expected).0),
        );
    }

    #[test]
    fn the_mapping_slot_separates_distinct_pairs_and_distinct_base_slots() {
        let pair = AddressPair::from_addresses(FIRST, SECOND);
        let other = AddressPair::from_addresses(FIRST, THIRD);

        assert_ne!(
            pair.mapping_slot(U256::from(10u64)),
            other.mapping_slot(U256::from(10u64))
        );
        assert_ne!(
            pair.mapping_slot(U256::from(10u64)),
            pair.mapping_slot(U256::from(11u64))
        );
    }

    #[test]
    fn the_mapping_slot_handles_the_zero_pair_and_the_zero_base_slot() {
        // The 40-byte key underflows the default 32-byte left-pad, so the
        // override is the only thing keeping this from panicking.
        let _ = AddressPair::ZERO.mapping_slot(U256::ZERO);
    }

    /// Pins the exact slot `scripts/seed_genesis.py` has to reproduce for the
    /// canonical COEN/840 pair at the pair-registry base slot. Its `mapping_key`
    /// helper already agrees: `rjust(32)` never truncates, so a 40-byte key
    /// falls through unpadded. A change here is genesis-breaking, not a refactor.
    #[test]
    fn the_coen_iso_840_pair_derives_a_stable_registry_slot() {
        let coen_usd = AddressPair::from_addresses(
            Address::ZERO,
            address!("0x00000000000000000000000000000000000cc840"),
        );

        assert_eq!(
            coen_usd.mapping_slot(U256::from(10u64)),
            U256::from_be_bytes(
                b256!("0xfa9240513fa8af0cd3aa94db0c237a129a63076f21ab227c30018c124938bc88").0
            ),
        );
    }
}
