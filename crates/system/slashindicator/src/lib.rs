//! Module-structure standard layout:
//! - `schema.rs` - storage schema for the `SlashIndicator` facade.
//! - `runtime.rs` - shared slashing helpers, felonies and getters.
//! - `misses.rs` - proposer/voter liveness misses.
//! - `equivocation.rs` - same-signer equivocation evidence.
//! - `vrf_slashing.rs` - invalid-VRF and seed-partial evidence.
//! - `evidence.rs` - byzantine evidence handling.
//! - `hooks.rs` - per-finalized-block guard wrappers.
//! - `precompile.rs` - ABI dispatch.
//!
//! `pub use` re-exports below preserve the old `contract` / `logic`
//! paths for external callers. Migrate those callers opportunistically.
mod equivocation;
mod evidence;
pub mod hooks;
pub mod metrics;
mod misses;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod seed_partial_evidence;
pub mod vrf_evidence;
mod vrf_slashing;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_signing;
#[cfg(test)]
mod tests;

pub mod contract {
    pub use crate::schema::*;
}
pub mod logic {
    pub use crate::runtime::*;
}
