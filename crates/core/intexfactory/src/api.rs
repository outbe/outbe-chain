//! Cross-module API for IntexFactory.
//!
//! `issue` is the clearing engine's (Desis) issuance hand-off — a Rust-to-Rust
//! call, not a precompile selector, mirroring Intex's write API. The
//! user-facing surface (settle / minePromis / setAuthorizedSettler) lives in
//! the precompile.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::config::{self, IntexParams};
use crate::runtime;
use crate::schema::{IntexFactoryContract, IssuanceParams};

/// Create a series and enroll it for autonomous qualification. Called by the
/// clearing engine after a cleared auction.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<()> {
    runtime::issue(storage, params)
}

/// Discard a day's contributor map. The clearing engine calls this for a day
/// that issued nothing at all: with no series anywhere, the recorded creator
/// rewards can never be distributed.
pub fn discard_day_contributors(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<()> {
    outbe_intex::api::finalize_proceeds(storage, worldwide_day)
}

/// Resolved IntexFactory protocol parameters (genesis profile). The clearing
/// engine (Desis) reads these to source floor%/call%/call-trigger at auction
/// start, keeping a single source of truth instead of hardcoding them.
pub fn read_params(storage: &StorageHandle<'_>) -> Result<IntexParams> {
    config::read(&IntexFactoryContract::new(storage.clone()))
}
