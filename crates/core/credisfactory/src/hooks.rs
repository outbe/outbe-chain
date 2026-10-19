//! Per-block entry points of the Credis sweeps, run from CycleTick.

/// Forfeits the called groups whose settlement window lapsed.
pub use crate::expired::sweep_expired as sweep_forfeits;

/// Deletes the Credis of the forfeited groups.
pub use crate::cleanup::sweep_cleanup;
