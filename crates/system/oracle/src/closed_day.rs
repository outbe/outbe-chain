use outbe_primitives::{
    error::Result,
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::schema::OracleContract;

/// The most recent fully-closed UTC day at `timestamp`, or `None` while its VWAPs are not final.
/// The begin-block hook finalizes that day earlier in the same block, so a lag means the ordering broke.
pub fn finalized_closed_day(
    storage: StorageHandle<'_>,
    timestamp: u64,
    consumer: &'static str,
) -> Result<Option<u32>> {
    let last_closed_day = previous_date_key(timestamp_to_date_key(timestamp));
    let finalized = OracleContract::new(storage)
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < last_closed_day {
        tracing::warn!(
            target: "outbe::oracle",
            consumer,
            last_closed_day,
            finalized,
            "utc-day VWAP not finalized yet, skipping the day's sweeps"
        );
        return Ok(None);
    }
    Ok(Some(last_closed_day))
}
