//! Oracle state fixtures for tests in other crates.
//!
//! Each function makes the same Oracle storage writes, in the same order, as
//! the Oracle API calls that it replaces. Each function returns the first
//! error. The caller decides how the test fails.

use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::api::{register_pair, set_exchange_rate, RateObservation};
use crate::constants::{DAY_TYPE_ISO, DAY_TYPE_PAIR};
use crate::schema::OracleContract;

/// Registers the COEN/840 pair. Then publishes `rate` as the COEN/840 rate
/// that the zero address observed in block `block_number` at `timestamp`.
pub fn publish_day_type_rate(
    storage: &StorageHandle<'_>,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    register_pair(storage.clone(), DAY_TYPE_PAIR)?;
    set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        DAY_TYPE_PAIR,
        RateObservation {
            rate,
            block_number,
            timestamp,
        },
    )
}

/// Does the steps of [`publish_day_type_rate`]. Then adds ISO 840 to the
/// reference currencies.
pub fn publish_day_type_quote(
    storage: &StorageHandle<'_>,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    publish_day_type_rate(storage, rate, block_number, timestamp)?;
    OracleContract::new(storage.clone())
        .reference_currencies
        .push(DAY_TYPE_ISO)
}
