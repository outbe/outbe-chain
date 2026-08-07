use alloy_primitives::{wrap_fixed_bytes, Address};
// `wrap_fixed_bytes!` expands to `derive_more` derives that emit unqualified
// paths, so the crate has to be nameable here.
use alloy_primitives::private::derive_more;

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
    pub fn new(first: Address, second: Address) -> Self {
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

#[cfg(test)]
mod tests {
    use super::AddressPair;
    use alloy_primitives::{address, Address};

    const FIRST: Address = address!("0x1111111111111111111111111111111111111111");
    const SECOND: Address = address!("0x2222222222222222222222222222222222222222");
    const THIRD: Address = address!("0x3333333333333333333333333333333333333333");

    #[test]
    fn from_addresses_packs_both_addresses_in_ascending_order() {
        let pair = AddressPair::new(FIRST, SECOND);

        assert_eq!(AddressPair::len_bytes(), 40);
        assert_eq!(&pair[0..20], FIRST.as_slice());
        assert_eq!(&pair[20..40], SECOND.as_slice());
    }

    #[test]
    fn the_accessors_return_the_packed_addresses_in_ascending_order() {
        let pair = AddressPair::new(SECOND, FIRST);

        assert_eq!(pair.first(), FIRST);
        assert_eq!(pair.second(), SECOND);
        assert_eq!(pair.pair(), (FIRST, SECOND));
    }

    #[test]
    fn the_accessors_round_trip_back_into_the_same_pair() {
        let pair = AddressPair::new(FIRST, SECOND);

        assert_eq!(
            AddressPair::new(pair.first(), pair.second()),
            pair
        );
    }

    #[test]
    fn from_addresses_ignores_the_argument_order() {
        assert_eq!(
            AddressPair::new(FIRST, SECOND),
            AddressPair::new(SECOND, FIRST),
        );
    }

    #[test]
    fn from_addresses_separates_pairs_sharing_an_address() {
        assert_ne!(
            AddressPair::new(FIRST, SECOND),
            AddressPair::new(FIRST, THIRD),
        );
    }

    #[test]
    fn from_addresses_packs_an_address_paired_with_itself() {
        let pair = AddressPair::new(FIRST, FIRST);

        assert_eq!(&pair[0..20], FIRST.as_slice());
        assert_eq!(&pair[20..40], FIRST.as_slice());
    }

    #[test]
    fn a_pair_round_trips_through_hex() {
        let pair = AddressPair::new(FIRST, SECOND);

        assert_eq!(pair.to_string().parse::<AddressPair>(), Ok(pair));
    }

    #[test]
    fn the_zero_pair_holds_forty_zero_bytes() {
        assert_eq!(
            AddressPair::new(Address::ZERO, Address::ZERO),
            AddressPair::ZERO,
        );
    }
}
