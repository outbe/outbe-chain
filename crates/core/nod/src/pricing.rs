//! Pure NOD price bounds and call-price bins.
use alloy_primitives::U256;
use outbe_primitives::call_bins::BIN_STEP_BP;
use outbe_primitives::{error::Result, math::reference_price};

/// Calculates the floor from the entry price. Returns `None` on overflow.
pub fn floor_price_minor(entry_price_minor: U256) -> Option<U256> {
    entry_price_minor
        .checked_mul(U256::from(100 + crate::constants::FLOOR_RATE_PCT))
        .map(|scaled| scaled / U256::from(100u64))
}

/// Whether the floor and the call price at any `u16` call rate fit `U256` for this entry.
pub fn is_issuable_entry(entry_price_minor: U256) -> bool {
    entry_price_minor
        .checked_mul(U256::from(100 + u32::from(u16::MAX)))
        .is_some()
}

/// Maps a six-decimal call price (or oracle rate) to a 24-bit
/// bin id on the LB log-spaced ladder. Saturates to `[0, MAX_BIN_ID]`.
/// See [`outbe_primitives::math::price_helper::get_id_from_price`] for the saturation
/// rationale.
pub fn price_to_bin(price_minor: U256) -> Result<u32> {
    if price_minor.is_zero() {
        return Ok(0);
    }
    reference_price::coen_iso_price_to_bin_id(price_minor, BIN_STEP_BP)
}

/// Inverse of `price_to_bin`: returns the lower edge of bin `bin_id` in
/// six-decimal minor units. Diagnostic-only. `bin_to_price_floor` may
/// fail at extreme bin ids whose LB-pow exponent exceeds `2^20`.
pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
    reference_price::bin_id_to_coen_iso_price(bin_id, BIN_STEP_BP)
}
