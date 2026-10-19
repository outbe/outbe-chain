//! Per-block entry points of the Nod sweeps, run from CycleTick.

/// Forfeits the called groups whose notice period lapsed.
pub use crate::expired::sweep_expired as sweep_forfeits;

/// Deletes the unpaid Nods of the forfeited groups.
pub use crate::cleanup::sweep_cleanup;
