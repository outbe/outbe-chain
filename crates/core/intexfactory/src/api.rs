//! Cross-module API for IntexFactory.
//!
//! `issue_intex` is the issuance hand-off of the clearing engine (Desis). It is a
//! Rust-to-Rust call, not a precompile selector, and it mirrors the Intex write
//! API. The user-facing surface (settle / minePromis) lives in the precompile.

use alloy_primitives::{Address, U256};
use outbe_intex::{IntexCallTrigger, SeriesId};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::{IntexCallTerms, IssuanceParams};

/// The call terms a series opened at `entry_price_minor` seals: the entry marked up by
/// the live profile's call rate, plus the profile's window, threshold and notice.
/// Desis calls this at the auction start and stores the answer with the day's conditions.
pub fn call_terms(storage: &StorageHandle<'_>, entry_price_minor: U256) -> Result<IntexCallTerms> {
    let profile = outbe_intex::config::read(storage)?;
    Ok(IntexCallTerms {
        call_price_minor: runtime::marked_up(entry_price_minor, profile.call_rate)?,
        call_trigger: IntexCallTrigger {
            call_window_seconds: profile.call_window_seconds,
            call_threshold_seconds: profile.call_threshold_seconds,
            call_notice_period_seconds: profile.call_notice_period_seconds,
        },
    })
}

/// Create a series and enroll it in the call-price index, returning what each
/// target chain must be told. The clearing engine calls this after a cleared auction,
/// with the conditions the day fixed at its auction start.
/// The clearing engine then packs the day's legs into messages and sends them.
pub fn issue_intex(
    storage: &StorageHandle<'_>,
    params: IssuanceParams,
) -> Result<Vec<runtime::IssuanceLeg>> {
    runtime::issue_intex(storage, params)
}

/// Send a day's issuance legs, packed into as few per-chain messages as the wire allows.
pub fn send_issuance(storage: &StorageHandle<'_>, legs: Vec<runtime::IssuanceLeg>) -> Result<()> {
    runtime::send_issuance(storage, legs)
}

/// The `minePromis` proof-of-work check, for off-chain miners.
pub fn validate_pow(
    owner: Address,
    promis_amount: U256,
    series_id: SeriesId,
    seq: u32,
    nonce: u64,
) -> Result<()> {
    runtime::validate_pow(owner, promis_amount, series_id, seq, nonce)
}
