//! Keys of the call-price bin index the rights' call sweeps walk.
//!
//! Each reference currency holds its own trie of bins on the Liquidity Book
//! ladder, and each bin a dense list of the entries priced into it.

use alloy_primitives::{keccak256, B256, U256};

use crate::error::Result;
use crate::math::reference_price;

/// Bin step of the call-price ladder, in basis points.
pub const BIN_STEP_BP: u16 = 25;

/// The bin a six-decimal COEN/ISO call price falls in. A zero price sits in bin 0.
pub fn price_to_bin(price: U256) -> Result<u32> {
    if price.is_zero() {
        return Ok(0);
    }
    reference_price::coen_iso_price_to_bin_id(price, BIN_STEP_BP)
}

/// The lower edge of `bin` as a six-decimal COEN/ISO price.
pub fn bin_to_price_floor(bin: u32) -> Result<U256> {
    reference_price::bin_id_to_coen_iso_price(bin, BIN_STEP_BP)
}

/// Namespaces a bin column key by its reference currency.
///
/// Mapping keys are left-padded to 32 bytes before hashing, so a wider integer
/// type alone namespaces nothing: the ISO has to occupy real high bits. Bin ids
/// are 24-bit and the trie's mid and leaf keys 16-bit, so the low 32 bits always
/// hold `key` unambiguously.
pub const fn scoped(reference_currency: u16, key: u32) -> u64 {
    ((reference_currency as u64) << 32) | key as u64
}

/// Key of the `index`-th entry in bin `bin` of `reference_currency`.
pub fn bin_index_key(reference_currency: u16, bin: u32, index: u32) -> B256 {
    let mut buf = [0u8; 10];
    buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
    buf[2..6].copy_from_slice(&bin.to_be_bytes());
    buf[6..10].copy_from_slice(&index.to_be_bytes());
    keccak256(buf)
}

/// An entry's place in the index: its bin and `index + 1`, so 0 means absent.
pub const fn pack_slot(bin: u32, index: u32) -> u64 {
    ((bin as u64) << 32) | (index as u64 + 1)
}

pub const fn unpack_slot(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed as u32).wrapping_sub(1))
}

/// A walk's place in a currency: the bin and the entries of it still to visit.
/// 0 entries left walks the bin from the top.
pub const fn pack_cursor(bin: u32, remaining: u32) -> u64 {
    ((bin as u64) << 32) | remaining as u64
}

pub const fn unpack_cursor(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_cursors_round_trip() {
        assert_eq!(pack_slot(7, 0), (7 << 32) | 1);
        assert_eq!(unpack_slot(pack_slot(7, 3)), (7, 3));
        assert_eq!(unpack_cursor(pack_cursor(9, 5)), (9, 5));
        assert_eq!(pack_cursor(0, 0), 0);
    }

    #[test]
    fn a_zero_price_sits_in_bin_zero_and_prices_ascend_by_bin() {
        assert_eq!(price_to_bin(U256::ZERO).unwrap(), 0);
        let low = price_to_bin(U256::from(1_000_000u64)).unwrap();
        let high = price_to_bin(U256::from(2_000_000u64)).unwrap();
        assert!(low < high);
        assert!(bin_to_price_floor(high).unwrap() <= U256::from(2_000_000u64));
    }

    #[test]
    fn keys_are_namespaced_by_currency() {
        assert_ne!(scoped(840, 1), scoped(978, 1));
        assert_ne!(bin_index_key(840, 1, 0), bin_index_key(978, 1, 0));
    }
}
