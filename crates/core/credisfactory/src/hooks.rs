//! Per-block entry points of the Credis sweeps, run from CycleTick.

/// Forfeits the called groups whose settlement window lapsed.
#[cfg(test)]
pub use crate::forfeited::sweep_expired as sweep_forfeits;

/// Deletes the Credis of the forfeited groups.
#[cfg(test)]
pub use crate::cleanup::sweep_cleanup;
