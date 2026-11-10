//! Typed errors for `outbe-consensus`.

pub mod dkg;

pub use self::dkg::*;
pub use outbe_simplex::errors::{core, follow, *};

mod engine_core;
mod stack_dkg;
mod stack_epoch;
mod stack_follower;
mod tee_bootstrap;
pub use engine_core::*;
pub use stack_dkg::StackDkgError;
pub use stack_epoch::StackEpochError;
pub use stack_follower::{
    FollowerStackError, StackRecoveryError, StackShutdownError, StackStartupError,
};
pub use tee_bootstrap::TeeBootstrapError;
