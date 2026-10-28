//! Per-block entry points of the Gem passes, run from CycleTick.

/// Forfeits the called groups whose notice period lapsed.
#[cfg(any(test, feature = "test-utils"))]
pub use crate::forfeited::sweep_expired as sweep_forfeits;

/// Deletes the unpaid gems of the forfeited groups.
pub use crate::cleanup::cleanup;

pub use crate::positions_expired::cleanup_positions;
/// Returns the unissued capacity of the hours whose positions expired, and deletes the
/// positions of the expired hours.
#[cfg(any(test, feature = "test-utils"))]
pub use crate::positions_expired::sweep_expired_positions;
