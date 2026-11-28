//! Cross-module API for Desis (Rust-to-Rust, not precompile selectors).
//!
//! Desis takes the Intex auction of each emission day in order after the day started, and the
//! auction starts by the date. Metadosis starts Desis of the day with its Desis limit later. The
//! schedule drives every stage. Capacity rejection is a typed
//! business result. Every technical or invariant failure remains an `Err`, with
//! all partial writes reverted. The auction key is the emission day: one
//! auction per day. Series ids are allocated at issuance.

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::EmissionDate;

use crate::constants::{CANCELED_LIMIT_OVERFLOW, CANCELED_NO_LIMIT};
use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::DesisContract;

/// Stable consensus reason why Desis returns a Desis limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DesisLimitRejectionReason {
    DesisLimitExceedsAuctionDomain = 1,
    AuctionClosed = 2,
}

impl DesisLimitRejectionReason {
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// Semantic result of one start of Desis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesisLimitReceipt {
    Accepted,
    ReturnedToPromisLimit {
        reason: DesisLimitRejectionReason,
        desis_limit_minor: U256,
        max_accepted: U256,
    },
}

/// The Desis limit of one day in raw PROMIS and whether the day has a limit to sell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesisLimit {
    pub emission_day: EmissionDate,
    pub desis_limit_minor: U256,
    pub has_limit: bool,
}

/// Cancel Desis of a day whose Metadosis failed. The next schedule step carries the cancel out.
/// A day with a result, or with none to wait for, does not change.
pub fn cancel_desis(storage: StorageHandle<'_>, emission_day: EmissionDate) -> Result<()> {
    runtime::request_cancel(&storage, emission_day, CANCELED_NO_LIMIT)
}

/// Starts Desis of the day with its Desis limit (raw PROMIS, has-limit flag). A limit outside
/// Desis' `u128` auction domain cancels the day, and a day that can no longer take a limit returns
/// it with no event. Both are committed returns.
/// Invalid state, timestamp overflow, storage/index/event faults and corruption propagate as
/// `Err`.
pub fn start_desis(storage: StorageHandle<'_>, limit: DesisLimit) -> Result<DesisLimitReceipt> {
    let DesisLimit {
        emission_day,
        desis_limit_minor,
        ..
    } = limit;
    storage.clone().with_checkpoint(|| {
        if !emission_day.is_valid() {
            return Err(crate::errors::DesisError::InvalidEmissionDay(emission_day).into());
        }
        if !runtime::takes_desis_limit(&storage, emission_day)? {
            return Ok(returned(
                DesisLimitRejectionReason::AuctionClosed,
                desis_limit_minor,
                U256::ZERO,
            ));
        }
        let mut contract = storage.contract::<DesisContract>();
        contract.emit(IDesis::DesisLimitSet {
            emissionDay: emission_day.into(),
            amountMinor: desis_limit_minor,
        })?;
        let Ok(desis_limit_u128) = u128::try_from(desis_limit_minor) else {
            runtime::record_desis_limit(&storage, limit, 0)?;
            runtime::request_cancel(&storage, emission_day, CANCELED_LIMIT_OVERFLOW)?;
            return Ok(returned(
                DesisLimitRejectionReason::DesisLimitExceedsAuctionDomain,
                desis_limit_minor,
                U256::from(u128::MAX),
            ));
        };
        runtime::record_desis_limit(&storage, limit, desis_limit_u128)?;
        Ok(DesisLimitReceipt::Accepted)
    })
}

fn returned(
    reason: DesisLimitRejectionReason,
    desis_limit_minor: U256,
    max_accepted: U256,
) -> DesisLimitReceipt {
    DesisLimitReceipt::ReturnedToPromisLimit {
        reason,
        desis_limit_minor,
        max_accepted,
    }
}
