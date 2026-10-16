//! The Oracle UTC day that prices a certified Lysis.

use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

/// The UTC day priced by a request: the last one closed before its block.
#[must_use]
pub fn entry_price_day(logical_evaluation_time: u64) -> u32 {
    previous_date_key(timestamp_to_date_key(logical_evaluation_time))
}
