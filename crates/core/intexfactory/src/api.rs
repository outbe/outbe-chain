//! Cross-module API for IntexFactory.
//!
//! `issue_intex` is the issuance hand-off of the clearing engine (Desis). It is a
//! Rust-to-Rust call, not a precompile selector, and it mirrors the Intex write
//! API. The user-facing surface (settle / minePromis) lives in the precompile.

use alloy_primitives::{Address, U256};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::IssuanceParams;

/// Create a series and enroll it in the call-price index, returning what each
/// target chain must be told. The clearing engine calls this after a cleared auction.
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
