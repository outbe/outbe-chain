//! CL/EL bridge over the consensus ports and the concrete execution node.

pub mod bridge;
pub mod errors;
pub mod execution_backend;
pub mod finalization;
pub mod stack;

pub use outbe_consensus::{
    application_shutdown, args, follower_shutdown, protocol_state,
    read_upstream_tribute_offer_public_key, validators,
};

pub use args::ConsensusArgs;
pub use stack::{
    run_consensus_stack, ConsensusExecutionServices, ConsensusRadicleServices,
    ConsensusShutdownServices, ConsensusStackServices,
};

pub use execution_backend::{run_consensus_with_execution, RethExecutionBackend};
