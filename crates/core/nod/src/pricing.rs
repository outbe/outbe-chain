//! Pure NOD price bounds and call-price bins.
use alloy_primitives::U256;
use outbe_primitives::call_bins::BIN_STEP_BP;
use outbe_primitives::markup::marked_up;
use outbe_primitives::{error::Result, math::reference_price};

/// Calculates the floor from the entry price. Returns `None` on overflow.
pub fn floor_price_minor(entry_price_minor: U256) -> Option<U256> {
    marked_up(entry_price_minor, crate::constants::FLOOR_RATE_PCT)
}

/// Whether the floor and the call price at any `u16` call rate fit `U256` for this entry.
pub use outbe_primitives::nod_rules::is_issuable_entry;

/// Inverse of `call_bins::price_to_bin`: returns the lower edge of bin `bin_id` in
/// six-decimal minor units. Diagnostic-only. `bin_to_price_floor` may
/// fail at extreme bin ids whose LB-pow exponent exceeds `2^20`.
pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
    reference_price::bin_id_to_coen_iso_price(bin_id, BIN_STEP_BP)
}
