//! Per-block entry points of the Nod sweeps, run from CycleTick.

/// Forfeits the called groups whose notice period lapsed.
#[cfg(any(test, feature = "test-utils"))]
pub use crate::forfeited::sweep_expired as sweep_forfeits;

/// Deletes the unpaid Nods of the forfeited groups.
pub use crate::cleanup::sweep_cleanup;
