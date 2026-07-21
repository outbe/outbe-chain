//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Called by Metadosis to drive auction lifecycle transitions. These entrypoints
//! are **best-effort**: a Desis-side failure surfaces as an `AuctionDispatchFailed`
//! event and a `false` return instead of halting the caller's block hook. The
//! caller (Metadosis) supplies only raw inputs; Desis owns config construction and
//! its own series-id derivation.
//!
//! The auction key is the worldwide day supplied by the caller — one auction
//! per day. `auction_timestamp` is only needed at stage start to derive the
//! auction windows. Series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{AuctionConfig, DesisContract};

/// Best-effort: create a new auction from the entry price and transition to
/// `Started`. Returns `true` if Desis accepted the signal.
pub fn dispatch_stage_start(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    auction_timestamp: u64,
    entry_price: U256,
) -> Result<bool> {
    let config = AuctionConfig::from_entry_price(entry_price);
    best_effort(storage, worldwide_day, "auction_stage_start", |s| {
        runtime::start_auction(s, worldwide_day, auction_timestamp, config)
    })
}

/// Best-effort: signal `Started` → `Revealing` (green day) or `Started` →
/// `Cancelled` (red day). Returns `true` if Desis accepted the signal.
pub fn dispatch_stage_reveal(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    is_green_day: bool,
) -> Result<bool> {
    best_effort(storage, worldwide_day, "auction_stage_reveal", |s| {
        runtime::reveal_auction(s, worldwide_day, is_green_day)
    })
}

/// Best-effort: signal `Revealing` → clearing stage with `supply_promis`.
///
/// Returns the PROMIS remainder the auction could **not** consume, which the
/// caller (Metadosis) routes back into the PromisLimit accumulator:
/// - on a delivered clearing, the rounding remainder `supply_promis %
///   promis_load_minor` (only whole `promis_load_minor` units can be auctioned);
/// - on a best-effort Desis failure, the **whole** `supply_promis`, so no budget
///   is lost.
///
/// The clearing **dispatch** does not write PromisLimit — returning the
/// remainder lets the caller own the accumulator write and avoids colliding with
/// the caller's own `set_total_unallocated`. (The asynchronous bid-settlement
/// path in `runtime::finalize`/`begin_clearing` still routes *unsold whole units*
/// back to PromisLimit; that is a separate flow.)
pub fn dispatch_stage_clearing(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    supply_promis: U256,
    now: u64,
) -> Result<U256> {
    let Ok(supply_u128) = u128::try_from(supply_promis) else {
        return clearing_failed(storage, worldwide_day, "supply exceeds u128", supply_promis);
    };

    match runtime::begin_clearing(storage.clone(), worldwide_day, supply_u128, now) {
        Ok(rounding_remainder) => Ok(U256::from(rounding_remainder)),
        Err(err) => clearing_failed(storage, worldwide_day, &format!("{err:?}"), supply_promis),
    }
}

/// Emit `AuctionDispatchFailed` for a clearing dispatch and return the whole
/// supply so the caller keeps the full budget (nothing cleared).
fn clearing_failed(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    reason: &str,
    supply_promis: U256,
) -> Result<U256> {
    let mut contract = storage.contract::<DesisContract>();
    contract.emit(IDesis::AuctionDispatchFailed {
        worldwideDay: worldwide_day,
        stage: "auction_stage_clearing".into(),
        reason: reason.into(),
    })?;
    Ok(supply_promis)
}

/// Run `f` against the storage handle; on error emit `AuctionDispatchFailed` and
/// return `Ok(false)` instead of propagating, so a Desis fault never halts the
/// caller's block hook.
fn best_effort(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    stage: &'static str,
    f: impl FnOnce(StorageHandle<'_>) -> Result<()>,
) -> Result<bool> {
    match f(storage.clone()) {
        Ok(()) => Ok(true),
        Err(err) => {
            let mut contract = storage.contract::<DesisContract>();
            contract.emit(IDesis::AuctionDispatchFailed {
                worldwideDay: worldwide_day,
                stage: stage.into(),
                reason: format!("{err:?}"),
            })?;
            Ok(false)
        }
    }
}
