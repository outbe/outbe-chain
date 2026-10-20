//! Per-block entry points of the Intex sweeps, run from CycleTick.

/// Expires the called groups whose notice period lapsed.
pub use crate::expired::sweep_expired as sweep_forfeits;

/// Deletes the spent series of the expired groups.
pub use crate::cleanup::sweep_cleanup;
