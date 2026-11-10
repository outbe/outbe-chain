//! Typed errors shared with the consensus stack.

pub use crate::finalization::FinalizationBarrierError;
pub use outbe_consensus::errors::{
    ApplicationShutdownError, ArgsError, FollowTransportError, FollowerShutdownError,
    FollowerStackError, PeerManagerError, StackDkgError, StackEpochError, StackRecoveryError,
    StackShutdownError, StackStartupError, TeeBootstrapError, ValidatorsError,
    ValidatorsPrecompileError,
};
