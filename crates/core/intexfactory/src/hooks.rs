//! Per-block entry points of the Intex sweeps, run from CycleTick.

/// Expires the called groups whose notice period lapsed.
#[cfg(test)]
pub use crate::forfeited::sweep_expired as sweep_forfeits;

/// Deletes the spent series of the expired groups.
pub use crate::cleanup::sweep_cleanup;
