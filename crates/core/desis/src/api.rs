//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Metadosis passes the day as a one-shot auction brief. From there, the Desis
//! begin-block schedule drives every stage. Capacity rejection is a typed
//! business result. Every technical or invariant failure remains an `Err`, with
//! all partial writes reverted. The auction key is the emission day: one
//! auction per day. Series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::EmissionDate;

use crate::constants::CANCELED_LIMIT_OVERFLOW;
use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{AuctionStage, DesisContract};

/// Stable consensus reason for the only business-level auction-brief rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AuctionBriefRejectionReason {
    DesisLimitExceedsAuctionDomain = 1,
}

impl AuctionBriefRejectionReason {
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// Semantic result of one auction-brief formation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuctionBriefReceipt {
    Accepted,
    ReturnedToPromisLimit {
        reason: AuctionBriefRejectionReason,
        desis_limit_minor: U256,
        max_accepted: U256,
    },
}

/// One day's auction brief: its limit in raw PROMIS and whether the day has a limit to sell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuctionBrief {
    pub emission_day: EmissionDate,
    pub desis_limit_minor: U256,
    pub has_limit: bool,
}

/// Record the day's auction brief (limit in raw PROMIS, has-limit flag). The day is
/// priced at auction start, not here. Only a limit outside Desis' `u128` auction
/// domain is a committed rejection. Invalid state, timestamp overflow,
/// storage/index/event faults and corruption propagate as `Err`.
pub fn dispatch_auction_brief(
    storage: StorageHandle<'_>,
    brief: AuctionBrief,
) -> Result<AuctionBriefReceipt> {
    let AuctionBrief {
        emission_day,
        desis_limit_minor,
        ..
    } = brief;
    storage.clone().with_checkpoint(|| {
        let anchor = runtime::preflight_brief(&storage, emission_day)?;
        let Ok(desis_limit_u128) = u128::try_from(desis_limit_minor) else {
            let max_accepted = U256::from(u128::MAX);
            let reason = AuctionBriefRejectionReason::DesisLimitExceedsAuctionDomain;
            let mut contract = storage.contract::<DesisContract>();
            contract.write_stage(emission_day, AuctionStage::Cancelled)?;
            contract.emit(IDesis::DesisLimitSet {
                emissionDay: emission_day.into(),
                amountMinor: desis_limit_minor,
            })?;
            contract.emit(IDesis::IntexAuctionCanceled {
                emissionDay: emission_day.into(),
                reason: CANCELED_LIMIT_OVERFLOW,
            })?;
            contract.emit(IDesis::DesisLimitAllocated {
                emissionDay: emission_day.into(),
                amountMinor: U256::ZERO,
            })?;
            contract.emit(IDesis::DesisLimitReturned {
                emissionDay: emission_day.into(),
                amountMinor: desis_limit_minor,
            })?;
            return Ok(AuctionBriefReceipt::ReturnedToPromisLimit {
                reason,
                desis_limit_minor,
                max_accepted,
            });
        };
        runtime::record_preflighted_brief(storage.clone(), brief, desis_limit_u128, anchor)?;
        storage
            .contract::<DesisContract>()
            .emit(IDesis::DesisLimitSet {
                emissionDay: emission_day.into(),
                amountMinor: desis_limit_minor,
            })?;
        Ok(AuctionBriefReceipt::Accepted)
    })
}
