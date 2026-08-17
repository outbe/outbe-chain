//! Strict OCOMP request-phase ownership boundary for Desis.
//!
//! The legacy cross-module API is deliberately best-effort because it is used
//! from a block hook. OCOMP request application instead needs an atomic,
//! fail-closed owner write whose exact input can be committed by the request
//! receipt.

use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_ocomp_protocol::intent::ReferenceEntryPriceV1;
use outbe_ocomp_protocol::receipts::desis_request_brief_hash;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::ReferenceCurrencyPrice;

/// Apply the day's immutable `auction_base` and return the canonical hash
/// committed by `RequestBudgetSplitReceiptV1`. A red day briefs no supply, but
/// is briefed all the same so its targets learn the auction is cancelled.
pub fn apply_request_auction_base(
    storage: StorageHandle<'_>,
    protocol_bundle_hash: B256,
    worldwide_day: WorldwideDay,
    auction_base: U256,
    auction_entry_prices: &[ReferenceEntryPriceV1],
    logical_anchor: u64,
    green: bool,
) -> Result<B256> {
    let briefed_supply = if green { auction_base } else { U256::ZERO };
    let supply_u128 = u128::try_from(briefed_supply).map_err(|_| {
        PrecompileError::Revert("OCOMP auction_base exceeds Desis u128 supply".into())
    })?;
    let brief_hash = desis_request_brief_hash(
        protocol_bundle_hash,
        worldwide_day.value(),
        briefed_supply,
        auction_entry_prices,
        logical_anchor,
    )
    .map_err(|error| PrecompileError::Revert(format!("invalid OCOMP Desis brief hash: {error}")))?;
    storage.with_checkpoint(|| {
        runtime::record_brief(
            storage.clone(),
            worldwide_day,
            supply_u128,
            auction_entry_prices
                .iter()
                .map(|row| ReferenceCurrencyPrice {
                    iso_code: row.reference_currency,
                    entry_price_minor: row.entry_price_minor,
                })
                .collect(),
            green,
            logical_anchor,
        )
    })?;
    Ok(brief_hash)
}
