//! Defines typed error enums for the engine crate.

mod core;
mod stack_dkg;
mod stack_epoch;
mod stack_follower;
mod tee_bootstrap;

pub use crate::ce_recovery::CeStartupRecoveryError;
pub use core::{
    validator_has_no_node_host_enclave_binding_error, ApplicationShutdownError, ArgsError,
    CeFinalizerError, CeRecoveryError, FollowTransportError, FollowerShutdownError,
    PeerManagerError, ValidatorsError,
};
pub use stack_dkg::StackDkgError;
pub use stack_epoch::StackEpochError;
pub use stack_follower::{
    FollowerStackError, StackRecoveryError, StackShutdownError, StackStartupError,
};
pub use tee_bootstrap::TeeBootstrapError;
