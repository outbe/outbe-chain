//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Metadosis passes the day as a one-shot auction brief. From there, the Desis
//! begin-block schedule drives every stage. Capacity rejection is a typed
//! business result. Every technical or invariant failure remains an `Err`, with
//! all partial writes reverted. The auction key is the worldwide day: one
//! auction per day. Series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::DesisContract;

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

/// How callers handle an oversized auction limit. Settlement can return it to
/// the Promis Limit. Native Lysis requires the prepared brief to succeed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BriefOverflowPolicy {
    ReturnToPromisLimit,
    Reject,
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
    pub worldwide_day: WorldwideDay,
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
    now: u64,
    overflow: BriefOverflowPolicy,
) -> Result<AuctionBriefReceipt> {
    let AuctionBrief {
        worldwide_day,
        desis_limit_minor,
        has_limit,
    } = brief;
    storage.clone().with_checkpoint(|| {
        if worldwide_day.value() == 0 && !has_limit && desis_limit_minor.is_zero() {
            let mut contract = storage.contract::<DesisContract>();
            contract.emit(IDesis::AuctionCancelledNoLimit {
                worldwideDay: worldwide_day.into(),
            })?;
            return Ok(AuctionBriefReceipt::Accepted);
        }
        let anchor = runtime::preflight_brief(&storage, worldwide_day, now)?;
        let Ok(desis_limit_u128) = u128::try_from(desis_limit_minor) else {
            let max_accepted = U256::from(u128::MAX);
            let reason = AuctionBriefRejectionReason::DesisLimitExceedsAuctionDomain;
            if matches!(overflow, BriefOverflowPolicy::Reject) {
                return Err(crate::errors::DesisError::AuctionBriefLimitExceedsDomain.into());
            }
            let mut contract = storage.contract::<DesisContract>();
            contract.emit(IDesis::AuctionBriefRejected {
                worldwideDay: worldwide_day.into(),
                desisLimitMinor: desis_limit_minor,
                maxAcceptedMinor: max_accepted,
                reasonCode: reason.code(),
            })?;
            return Ok(AuctionBriefReceipt::ReturnedToPromisLimit {
                reason,
                desis_limit_minor,
                max_accepted,
            });
        };
        runtime::record_preflighted_brief(
            storage.clone(),
            worldwide_day,
            desis_limit_u128,
            has_limit,
            anchor,
        )?;
        Ok(AuctionBriefReceipt::Accepted)
    })
}
