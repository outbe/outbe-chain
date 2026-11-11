//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Metadosis passes the day as a one-shot auction brief. From there, the Desis
//! begin-block schedule drives every stage. Every failure is an `Err`, with all
//! partial writes reverted. The auction key is the emission day: one
//! auction per day. Series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::EmissionDate;

use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::DesisContract;

/// One day's auction brief: its limit in raw PROMIS and whether the day has a limit to sell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuctionBrief {
    pub emission_day: EmissionDate,
    pub desis_limit_minor: U256,
    pub has_limit: bool,
}

/// Record the day's auction brief (limit in raw PROMIS, has-limit flag). The day is
/// priced at auction start, not here. A limit outside Desis' `u128` auction domain,
/// invalid state, timestamp overflow, storage/index/event faults and corruption
/// propagate as `Err`.
pub fn dispatch_auction_brief(storage: StorageHandle<'_>, brief: AuctionBrief) -> Result<()> {
    let AuctionBrief {
        emission_day,
        desis_limit_minor,
        ..
    } = brief;
    storage.clone().with_checkpoint(|| {
        let anchor = runtime::preflight_brief(&storage, emission_day)?;
        let desis_limit_u128 = u128::try_from(desis_limit_minor)
            .map_err(|_| crate::errors::DesisError::AuctionBriefLimitExceedsDomain)?;
        runtime::record_preflighted_brief(storage.clone(), brief, desis_limit_u128, anchor)?;
        storage
            .contract::<DesisContract>()
            .emit(IDesis::DesisLimitSet {
                emissionDay: emission_day.into(),
                amountMinor: desis_limit_minor,
            })
    })
}
