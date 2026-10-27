//! `HyperlaneController` - governance-owned controller of the Hyperlane bridge (`0x...EE14`).
//!
//! The precompile owns every Hyperlane core contract on Outbe (Mailbox,
//! ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
//! StorageMessageIdMultisigIsm). Through its Interchain Account, it also owns
//! the same contracts on remote chains. It holds no validator set of its own.
//! The ISMs are the source of truth. The controller only forwards owner calls.
//!
//! - Direct write selectors are `initialize`, `fund`, permissionless `sync`,
//!   `setHyperlaneSigner`, and `submitCheckpoint`.
//! - `sync` rotates validators through `set_validators_and_threshold`.
//!   Begin-block liveness calls the same rotation.
//! - `CrosschainVoteTarget` stores the hash of an approved owner action.
//!   Permissionless `applyApproved` then runs it through `call_local`,
//!   `call_remote`, `add_domain`, or `remove_domain`, with the controller as
//!   `msg.sender`. The vote tally runs where sub-calls are not available.
//! - The controller pays remote dispatch fees (IGP quote) from its own
//!   balance. `fund` adds value to that balance.

pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod schema;

mod ism_sync;
mod liveness;
mod remote;
mod runtime;
mod sol_ext;
mod vote_target;

pub use liveness::{
    checkpoint_digest, SignedCheckpoint, GRACE_BLOCKS, LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use remote::RemoteCall;
pub use runtime::{consensus_threshold, validate_validators, ControllerBootstrap};
pub use schema::HyperlaneControllerContract;
pub use vote_target::CrosschainVoteTarget;

#[cfg(test)]
mod tests;
