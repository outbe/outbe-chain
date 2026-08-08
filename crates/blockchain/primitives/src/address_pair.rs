use alloy_primitives::{keccak256, wrap_fixed_bytes, Address, U256};
// `wrap_fixed_bytes!` expands to `derive_more` derives that emit unqualified
// paths, so the crate has to be nameable here.
use crate::asset_type::AssetType;
use crate::storage::types::StorageKey;
use alloy_primitives::private::derive_more;

wrap_fixed_bytes!(
    /// Two Ethereum addresses concatenated, 40 bytes in length.
    ///
    /// Declared with the [`wrap_fixed_bytes!`] macro like [`Address`] itself,
    /// so it inherits hex parsing, formatting, and `FixedBytes` conversions.
    pub struct AddressPair<40>;
);

impl AddressPair {
    /// Packs the pair as quoted: `base` in bytes `0..20`, `quote` in `20..40`.
    ///
    /// The orientation is kept verbatim, because it is what the ABI reports back
    /// and what separates a rate from its reciprocal. Storage lookup stays
    /// order-independent regardless — [`StorageKey::key_bytes`] sorts on the way
    /// to a slot — so no caller has to canonicalize by hand.
    ///
    /// `new` is already taken by the raw `[u8; 40]` constructor that
    /// [`wrap_fixed_bytes!`] generates.
    pub fn quoted(base: Address, quote: Address) -> Self {
        let mut bytes = [0u8; 40];
        bytes[0..20].copy_from_slice(base.as_slice());
        bytes[20..40].copy_from_slice(quote.as_slice());
        Self::from(bytes)
    }

    /// [`Self::quoted`] over the asset encoding rather than raw addresses.
    pub fn quoted_assets(base: AssetType, quote: AssetType) -> Self {
        Self::quoted(base.into(), quote.into())
    }

    /// The asset being priced.
    pub fn base(&self) -> Address {
        Address::from_slice(&self[0..20])
    }

    /// The asset it is priced in.
    pub fn quote(&self) -> Address {
        Address::from_slice(&self[20..40])
    }

    /// The 40 bytes this pair keys on: both addresses ascending, so the two
    /// directions of one market collapse onto a single slot. Idempotent.
    pub fn sorted(&self) -> Self {
        let (base, quote) = (self.base(), self.quote());
        if base <= quote {
            *self
        } else {
            Self::quoted(quote, base)
        }
    }

    /// Whether both quotes name the same market, in either direction.
    ///
    /// `==` is direction-*sensitive* while storage lookup is not. A scan for
    /// "the entry belonging to this market" wants this; a guard asserting "quoted
    /// the way it was registered" wants `==`. Choosing wrong fails silently — an
    /// entry that reads as absent, or a reciprocal rate accepted as genuine.
    pub fn same_market(&self, other: &Self) -> bool {
        self.sorted() == other.sorted()
    }
}

/// Usable as a `Mapping` key, and as a value only through the two-word
/// `Mapping<K, AddressPair>` accessors: [`Storable`] round-trips through a
/// single 32-byte word and 40 bytes do not fit one.
///
/// [`Storable`]: crate::storage::types::Storable
impl StorageKey for AddressPair {
    /// Sorted, so `quoted(a, b)` and `quoted(b, a)` address the same slot.
    fn key_bytes(&self) -> Vec<u8> {
        self.sorted().as_slice().to_vec()
    }

    /// Solidity left-pads a mapping key only when it is narrower than a word;
    /// a wider key is concatenated with the base slot as-is. The provided
    /// implementation computes `32 - key.len()`, which a 40-byte key underflows,
    /// so the concatenation is spelled out here against a fixed-size buffer.
    fn mapping_slot(&self, base_slot: U256) -> U256 {
        let mut buf = [0u8; 72];
        buf[..40].copy_from_slice(self.sorted().as_slice());
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
    fn quoted_packs_base_first_and_quote_second() {
        let pair = AddressPair::quoted(SECOND, FIRST);

        assert_eq!(AddressPair::len_bytes(), 40);
        assert_eq!(&pair[0..20], SECOND.as_slice());
        assert_eq!(&pair[20..40], FIRST.as_slice());
    }

    #[test]
    fn the_accessors_return_the_quoted_orientation() {
        let pair = AddressPair::quoted(SECOND, FIRST);

        assert_eq!(pair.base(), SECOND);
        assert_eq!(pair.quote(), FIRST);
    }

    #[test]
    fn the_accessors_round_trip_back_into_the_same_pair() {
        let pair = AddressPair::quoted(SECOND, FIRST);

        assert_eq!(AddressPair::quoted(pair.base(), pair.quote()), pair);
    }

    #[test]
    fn quoted_keeps_the_two_directions_of_one_market_distinct() {
        assert_ne!(
            AddressPair::quoted(FIRST, SECOND),
            AddressPair::quoted(SECOND, FIRST),
        );
    }

    #[test]
    fn sorted_collapses_the_two_directions_of_one_market() {
        let forward = AddressPair::quoted(FIRST, SECOND);
        let reverse = AddressPair::quoted(SECOND, FIRST);

        assert_eq!(forward.sorted(), reverse.sorted());
        assert!(forward.same_market(&reverse));
        // Idempotent: sorting an already-sorted pair is a no-op.
        assert_eq!(forward.sorted().sorted(), forward.sorted());
        assert_eq!(forward.sorted(), forward);
    }

    #[test]
    fn same_market_separates_markets_sharing_an_asset() {
        assert!(!AddressPair::quoted(FIRST, SECOND).same_market(&AddressPair::quoted(FIRST, THIRD)));
    }

    #[test]
    fn quoted_separates_pairs_sharing_an_address() {
        assert_ne!(
            AddressPair::quoted(FIRST, SECOND),
            AddressPair::quoted(FIRST, THIRD),
        );
    }

    #[test]
    fn quoted_packs_an_address_paired_with_itself() {
        let pair = AddressPair::quoted(FIRST, FIRST);

        assert_eq!(&pair[0..20], FIRST.as_slice());
        assert_eq!(&pair[20..40], FIRST.as_slice());
    }

    #[test]
    fn a_pair_round_trips_through_hex() {
        let pair = AddressPair::quoted(FIRST, SECOND);

        assert_eq!(pair.to_string().parse::<AddressPair>(), Ok(pair));
    }

    #[test]
    fn the_zero_pair_holds_forty_zero_bytes() {
        assert_eq!(
            AddressPair::quoted(Address::ZERO, Address::ZERO),
            AddressPair::ZERO,
        );
    }

    #[test]
    fn the_mapping_slot_concatenates_the_forty_byte_key_with_the_base_slot() {
        let pair = AddressPair::quoted(FIRST, SECOND);
        let base_slot = U256::from(10u64);

        let mut expected = Vec::with_capacity(72);
        expected.extend_from_slice(pair.as_slice());
        expected.extend_from_slice(&base_slot.to_be_bytes::<32>());

        assert_eq!(
            pair.mapping_slot(base_slot),
            U256::from_be_bytes(keccak256(&expected).0),
        );
    }

    /// The one property that lets a single `Mapping<AddressPair, _>` serve both
    /// quote directions: the key derivation sorts even though the value does not.
    #[test]
    fn the_mapping_slot_ignores_the_quote_direction() {
        let base_slot = U256::from(10u64);

        assert_eq!(
            AddressPair::quoted(FIRST, SECOND).mapping_slot(base_slot),
            AddressPair::quoted(SECOND, FIRST).mapping_slot(base_slot),
        );
    }

    #[test]
    fn the_mapping_slot_separates_distinct_pairs_and_distinct_base_slots() {
        let pair = AddressPair::quoted(FIRST, SECOND);
        let other = AddressPair::quoted(FIRST, THIRD);

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
        let coen_usd = AddressPair::quoted(
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
