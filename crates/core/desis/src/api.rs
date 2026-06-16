//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Called by Metadosis to drive auction lifecycle transitions.
//!
//! The `auction_date` parameter is a yyyymmdd date key. Desis currently
//! uses it directly as `series_id`; the mapping will become non-trivial
//! once a single day can host multiple series.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;

use crate::errors::DesisError;
use crate::runtime;
use crate::schema::AuctionConfig;

/// Create a new auction for `auction_date` and transition to `Started`.
pub fn start_auction(
    storage: StorageHandle<'_>,
    auction_date: u32,
    config: AuctionConfig,
) -> Result<()> {
    runtime::start_auction(storage, auction_date, config)
}

/// Signal `Started` → `Revealing` (green day) or `Started` → `Cancelled` (red day).
pub fn reveal_auction(
    storage: StorageHandle<'_>,
    auction_date: u32,
    is_green_day: bool,
) -> Result<()> {
    runtime::reveal_auction(storage, auction_date, is_green_day)
}

/// Signal `Revealing` → clearing stage.
/// Rounding remainder (supply_promis % promis_load_minor) is returned to PromisLimit.
pub fn begin_clearing(
    storage: StorageHandle<'_>,
    auction_date: u32,
    supply_promis: U256,
) -> Result<()> {
    let supply_u128 =
        u128::try_from(supply_promis).map_err(|_| DesisError::InvalidSeriesId(auction_date))?;
    let remainder = runtime::begin_clearing(storage.clone(), auction_date, supply_u128)?;
    if remainder > 0 {
        PromisLimitContract::new(storage).add_to_total_unallocated(U256::from(remainder))?;
    }
    Ok(())
}
