//! Fidelity (RCFI) precompile (`0x100C`).
//!
//! Cohorts share the enclave-resident Gratis ledger and encrypted global journal.
//! Owner-authorized encrypted queries return view-key encrypted receipts; internal
//! league snapshots retain the shared fixed-point RCFI arithmetic.

pub mod api;
pub mod enclave_client;
pub mod precompile;
pub mod schema;

pub(crate) mod runtime;
pub(crate) mod state;

/// Shared fixed-point RCFI decay math - re-exported so existing
/// `crate::math::*` paths keep working after the arithmetic moved to the leaf
/// crate `outbe-fidelity-math` (also used by the enclave engine).
pub use outbe_fidelity_math as math;

pub use outbe_fidelity_math::{MAX_LEAGUE, MIN_LEAGUE};
pub use schema::FidelityContract;

#[cfg(test)]
mod tests;
