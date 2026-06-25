//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Called by Metadosis to drive auction lifecycle transitions. These entrypoints
//! are **best-effort**: a Desis-side failure surfaces as an `AuctionDispatchFailed`
//! event and a `false` return instead of halting the caller's block hook. The
//! caller (Metadosis) supplies only raw inputs; Desis owns config construction and
//! its own series-id derivation.
//!
//! The `auction_timestamp` parameter is the worldwide day's scheduled-process unix
//! timestamp. Desis derives the auction series id as
//! `timestamp_to_date_key(auction_timestamp)` (a yyyymmdd date key) and currently
//! uses it directly as the series id; the mapping will become non-trivial once a
//! single day can host multiple series.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::timestamp_to_date_key;
use outbe_promislimit::PromisLimitContract;

use crate::errors::DesisError;
use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{AuctionConfig, DesisContract};

/// Best-effort: create a new auction from the entry price and transition to
/// `Started`. Returns `true` if Desis accepted the signal.
pub fn dispatch_stage_start(
    storage: StorageHandle<'_>,
    auction_timestamp: u64,
    entry_price: U256,
) -> Result<bool> {
    let series_id = timestamp_to_date_key(auction_timestamp);
    let config = AuctionConfig::from_entry_price(entry_price);
    best_effort(storage, series_id, "auction_stage_start", |s| {
        runtime::start_auction(s, series_id, config)
    })
}

/// Best-effort: signal `Started` → `Revealing` (green day) or `Started` →
/// `Cancelled` (red day). Returns `true` if Desis accepted the signal.
pub fn dispatch_stage_reveal(
    storage: StorageHandle<'_>,
    auction_timestamp: u64,
    is_green_day: bool,
) -> Result<bool> {
    let series_id = timestamp_to_date_key(auction_timestamp);
    best_effort(storage, series_id, "auction_stage_reveal", |s| {
        runtime::reveal_auction(s, series_id, is_green_day)
    })
}

/// Best-effort: signal `Revealing` → clearing stage with `supply_promis`.
/// The rounding remainder (`supply_promis % promis_load_minor`) is returned to
/// PromisLimit. Returns `true` if Desis accepted the signal; on `false` the caller
/// routes the whole supply to PromisLimit so no budget is lost.
pub fn dispatch_stage_clearing(
    storage: StorageHandle<'_>,
    auction_timestamp: u64,
    supply_promis: U256,
) -> Result<bool> {
    let series_id = timestamp_to_date_key(auction_timestamp);
    best_effort(storage, series_id, "auction_stage_clearing", |s| {
        let supply_u128 =
            u128::try_from(supply_promis).map_err(|_| DesisError::InvalidSeriesId(series_id))?;
        let remainder = runtime::begin_clearing(s.clone(), series_id, supply_u128)?;
        if remainder > 0 {
            PromisLimitContract::new(s).add_to_total_unallocated(U256::from(remainder))?;
        }
        Ok(())
    })
}

/// Run `f` against the storage handle; on error emit `AuctionDispatchFailed` and
/// return `Ok(false)` instead of propagating, so a Desis fault never halts the
/// caller's block hook.
fn best_effort(
    storage: StorageHandle<'_>,
    series_id: u32,
    stage: &'static str,
    f: impl FnOnce(StorageHandle<'_>) -> Result<()>,
) -> Result<bool> {
    match f(storage.clone()) {
        Ok(()) => Ok(true),
        Err(err) => {
            let mut contract = storage.contract::<DesisContract>();
            contract.emit(IDesis::AuctionDispatchFailed {
                seriesId: series_id,
                stage: stage.into(),
                reason: format!("{err:?}"),
            })?;
            Ok(false)
        }
    }
}
