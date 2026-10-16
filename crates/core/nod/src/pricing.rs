//! Pure NOD price bounds and call-price bins.
use alloy_primitives::U256;
use outbe_primitives::call_bins::BIN_STEP_BP;
use outbe_primitives::markup::marked_up;
use outbe_primitives::{error::Result, math::reference_price};

/// Whether the call price at any `u16` call rate fits `U256` for this entry.
pub fn is_issuable_entry(entry_price_minor: U256) -> bool {
    marked_up(entry_price_minor, u16::MAX).is_some()
}

/// Inverse of `call_bins::price_to_bin`: returns the lower edge of bin `bin_id` in
/// six-decimal minor units. Diagnostic-only. `bin_to_price_floor` may
/// fail at extreme bin ids whose LB-pow exponent exceeds `2^20`.
pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
    reference_price::bin_id_to_coen_iso_price(bin_id, BIN_STEP_BP)
}
