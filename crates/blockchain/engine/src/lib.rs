//! Bridge between the consensus stack and concrete execution-node services.
//!
//! The CL stack remains here until its staged relocation is complete. Base
//! CL modules are re-exported from consensus to preserve existing import paths.

pub mod bridge;
pub mod errors;
pub mod finalization;
pub mod stack;

pub use outbe_consensus::{
    application_shutdown, args, follower_shutdown, marshal_update_reporter, peer_manager,
    protocol_state, read_upstream_tribute_offer_public_key, tee_bootstrap, validators,
};

/// Compatibility adapter until the CL stack is moved to consensus.
pub mod follow_transport {
    pub use outbe_consensus::follow_transport::*;
    pub type RethLocalBlockSource<N = outbe_execution::OutbeFullNode> =
        outbe_consensus::follow_transport::RethLocalBlockSource<N>;
}

pub use args::ConsensusArgs;
pub use stack::{
    run_consensus_stack, ConsensusExecutionServices, ConsensusRadicleServices,
    ConsensusShutdownServices, ConsensusStackServices,
};
