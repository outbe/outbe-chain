//! Cross-module API for IntexFactory.
//!
//! `issue` is the issuance hand-off of the clearing engine (Desis). It is a
//! Rust-to-Rust call, not a precompile selector, and it mirrors the Intex write
//! API. The user-facing surface (settle / minePromis) lives in the precompile.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use crate::config::{self, IntexParams};
use crate::runtime;
use crate::schema::IssuanceParams;

/// Create a series and enroll it in the call-price index, returning what each
/// target chain must be told. The clearing engine calls this after a cleared auction.
/// The clearing engine then packs the day's legs into messages and sends them.
pub fn issue(
    storage: &StorageHandle<'_>,
    params: IssuanceParams,
) -> Result<Vec<runtime::IssuanceLeg>> {
    runtime::issue(storage, params)
}

/// Send a day's issuance legs, packed into as few per-chain messages as the wire allows.
pub fn send_issuance(storage: &StorageHandle<'_>, legs: Vec<runtime::IssuanceLeg>) -> Result<()> {
    runtime::send_issuance(storage, legs)
}

/// Close a day's proceeds aggregation. The clearing engine calls this for a
/// day that issued nothing at all: with no series anywhere, no proceeds can
/// arrive to pay out.
pub fn discard_day_contributors(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    outbe_intex::api::finalize_proceeds(storage, worldwide_day)
}

/// Resolved IntexFactory protocol parameters (genesis profile). The clearing
/// engine (Desis) reads these at auction start to source floor%/call%/call-trigger.
/// This keeps a single source of truth instead of hardcoded values.
pub fn read_params(storage: &StorageHandle<'_>) -> Result<IntexParams> {
    config::read(storage)
}
