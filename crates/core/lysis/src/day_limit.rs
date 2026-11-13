//! The Lysis Limit of an emission day.

use alloy_primitives::U256;

/// Share of the priced Tribute nominal that Lysis may load, in percent.
pub const SYMBOLIC_RATE: u64 = 32;

/// Returns `(demand, lysis_limit)`: `demand = floor(nominal * SYMBOLIC_RATE / 100)` and
/// `lysis_limit = min(demand, metadosis_limit)`, or `None` on overflow.
pub fn lysis_limit_of_day(nominal: U256, metadosis_limit: U256) -> Option<(U256, U256)> {
    let denominator = U256::from(100u64);
    let rate = U256::from(SYMBOLIC_RATE);
    let demand = (nominal / denominator)
        .checked_mul(rate)?
        .checked_add((nominal % denominator).checked_mul(rate)? / denominator)?;
    Some((demand, demand.min(metadosis_limit)))
}
