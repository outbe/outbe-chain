//! Strict OCOMP request-phase ownership boundary for Desis.
//!
//! The legacy cross-module API is deliberately best-effort because it is used
//! from a block hook. OCOMP request application instead needs an atomic,
//! fail-closed owner write whose exact input can be committed by the request
//! receipt.

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::receipts::desis_request_brief_hash;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::ReferencePrice;

/// Apply a GREEN day's immutable `auction_base` and return the canonical hash
/// committed by `RequestBudgetSplitReceiptV1`.
pub fn apply_request_auction_base(
    storage: StorageHandle<'_>,
    protocol_bundle_hash: B256,
    worldwide_day: u32,
    auction_base: U256,
    auction_entry_price: U256,
    logical_anchor: u64,
) -> Result<B256> {
    let supply_u128 = u128::try_from(auction_base).map_err(|_| {
        PrecompileError::Revert("OCOMP auction_base exceeds Desis u128 supply".into())
    })?;
    let brief_hash = desis_request_brief_hash(
        protocol_bundle_hash,
        worldwide_day,
        auction_base,
        auction_entry_price,
        logical_anchor,
    )
    .map_err(|error| PrecompileError::Revert(format!("invalid OCOMP Desis brief hash: {error}")))?;
    storage.with_checkpoint(|| {
        runtime::record_brief(
            storage.clone(),
            worldwide_day,
            supply_u128,
            // The OCOMP brief carries one price, and it is part of the request hash.
            vec![ReferencePrice {
                iso_code: outbe_intexfactory::constants::QUALIFIER_REFERENCE_ISO,
                entry_price_minor: auction_entry_price,
            }],
            true,
            logical_anchor,
        )
    })?;
    Ok(brief_hash)
}
