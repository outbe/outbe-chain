//! Cross-module API for IntexFactory.
//!
//! `issue` is the clearing engine's (Desis) issuance hand-off — a Rust-to-Rust
//! call, not a precompile selector, mirroring Intex's write API. The
//! user-facing surface (settle / minePromis / setAuthorizedSettler) lives in
//! the precompile.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::IssuanceParams;

/// Create a series and enroll it for autonomous qualification. Called by the
/// clearing engine after a cleared auction.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<()> {
    runtime::issue(storage, params)
}
