//! CL/EL bridge over the consensus ports and the concrete execution node.

pub mod bridge;
pub mod errors;
pub mod execution_backend;
pub mod finalization;
pub mod stack;

pub use execution_backend::{run_consensus_with_execution, RethExecutionBackend};
