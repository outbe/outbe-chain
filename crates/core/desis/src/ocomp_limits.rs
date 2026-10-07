//! Strict OCOMP request-phase ownership boundary for Desis.
//!
//! Both settlement and OCOMP use the checkpointed `dispatch_auction_brief` API.
//! OCOMP rejects overflow so the request receipt commits the exact accepted input.
//! Both paths propagate technical and invariant failures.

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::receipts::desis_request_brief_hash;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::api::AuctionBrief;

/// Apply the day's immutable `desis_limit_minor` and return the canonical hash
/// committed by `RequestLimitSplitReceiptV1`. A red day briefs no limit, but
/// is briefed all the same so its targets learn the auction is cancelled.
pub fn apply_request_desis_limit(
    storage: StorageHandle<'_>,
    protocol_bundle_hash: B256,
    brief: AuctionBrief,
    logical_anchor: u64,
) -> Result<B256> {
    let AuctionBrief {
        worldwide_day,
        desis_limit_minor,
        is_green: green,
    } = brief;
    let desis_limit_minor = if green { desis_limit_minor } else { U256::ZERO };
    let brief_hash = desis_request_brief_hash(
        protocol_bundle_hash,
        worldwide_day.value(),
        desis_limit_minor,
        logical_anchor,
    )
    .map_err(|error| PrecompileError::Revert(format!("invalid OCOMP Desis brief hash: {error}")))?;
    // Same door as the settlement paths. Only the overflow policy differs,
    // because this receipt commits a hash a rejection could not fill.
    crate::api::dispatch_auction_brief(
        storage,
        AuctionBrief {
            worldwide_day,
            desis_limit_minor,
            is_green: green,
        },
        logical_anchor,
        crate::api::BriefOverflowPolicy::Reject,
    )?;
    Ok(brief_hash)
}
