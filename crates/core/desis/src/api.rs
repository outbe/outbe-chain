//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Metadosis hands the day over as a one-shot auction brief; the Desis
//! begin-block schedule drives every stage from there. The entrypoint is
//! **best-effort**: a Desis-side failure surfaces as an `AuctionDispatchFailed`
//! event and a `false` return instead of halting the caller's block hook, with
//! every write reverted. The auction key is the worldwide day — one auction per
//! day; series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::DesisContract;

/// Best-effort: record the day's auction brief (supply in raw PROMIS, entry
/// price, day type). Returns `true` if Desis accepted it; on failure nothing
/// is recorded.
pub fn dispatch_auction_brief(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    supply_promis: U256,
    entry_price: U256,
    is_green: bool,
    now: u64,
) -> Result<bool> {
    let Ok(supply_u128) = u128::try_from(supply_promis) else {
        let mut contract = storage.contract::<DesisContract>();
        contract.emit(IDesis::AuctionDispatchFailed {
            worldwideDay: worldwide_day,
            stage: "auction_brief".into(),
            reason: "supply exceeds u128".into(),
        })?;
        return Ok(false);
    };
    best_effort(storage, worldwide_day, "auction_brief", |s| {
        runtime::record_brief(s, worldwide_day, supply_u128, entry_price, is_green, now)
    })
}

/// Run `f` under a storage checkpoint; on error revert its writes, emit
/// `AuctionDispatchFailed` (outside the checkpoint) and return `Ok(false)` instead
/// of propagating, so a Desis fault never halts the caller's block hook.
fn best_effort(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    stage: &'static str,
    f: impl FnOnce(StorageHandle<'_>) -> Result<()>,
) -> Result<bool> {
    match storage.with_checkpoint(|| f(storage.clone())) {
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
