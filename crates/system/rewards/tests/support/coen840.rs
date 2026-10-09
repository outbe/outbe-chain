//! COEN/840 Oracle fixtures shared by the Rewards unit and integration tests.
//!
//! Each test target includes this file with an explicit `#[path]`.

use alloy_primitives::U256;
use outbe_primitives::storage::StorageHandle;

/// One unit of the COEN/840 rate at the Oracle's 6-decimal scale.
pub fn one_coen840() -> U256 {
    U256::from(1_000_000u64)
}

/// Registers the COEN/840 pair and ISO 840 as a reference currency. Then
/// publishes `rate` as observed in block `block_number` at `timestamp`.
pub fn publish_coen840_quote(
    storage: &StorageHandle<'_>,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) {
    outbe_oracle::test_support::publish_day_type_quote(storage, rate, block_number, timestamp)
        .unwrap();
}

/// Records `vwap` as the finalized COEN/840 VWAP of UTC day `day`.
pub fn record_coen840_day_vwap(storage: &StorageHandle<'_>, day: u32, vwap: U256) {
    let (_, index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, vwap)
        .unwrap();
}
