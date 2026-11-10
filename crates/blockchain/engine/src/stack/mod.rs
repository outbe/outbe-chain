//! Execution-backed snapshot fixtures and CL/EL integration tests.

#[cfg(feature = "snapshot-integration")]
#[doc(hidden)]
pub mod snapshot_native;
#[cfg(test)]
mod tests;

// Kept until the runtime composition root migrates in Task 5.2.
pub use outbe_consensus::stack::{
    dkg::persistence::migrate_dkg_keys_if_needed, run_consensus_stack, ConsensusExecutionServices,
    ConsensusRadicleServices, ConsensusShutdownServices, ConsensusStackServices,
};
