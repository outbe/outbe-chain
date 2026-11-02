//! Hyperlane controller: the module that runs the Hyperlane infrastructure
//! for governance.
//!
//! The module is a library with no address of its own. It runs in the frame
//! of the CrosschainGov precompile (`outbe-crosschaingov`, `0x…EE14`). That
//! precompile owns every Hyperlane core contract on Outbe (Mailbox, ProxyAdmin,
//! IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
//! StorageMessageIdMultisigIsm). Through its Interchain Account, it also owns
//! the same contracts on remote chains. This module holds no validator set of
//! its own. The ISMs are the source of truth.
//!
//! - [`dispatch()`] runs the Hyperlane selectors that the precompile sends on:
//!   `initialize`, permissionless `sync`, `setHyperlaneSigner`,
//!   `submitCheckpoint` and the views.
//! - `sync` rotates validators through `set_validators_and_threshold`.
//!   Begin-block liveness calls the same rotation.
//! - [`HyperlaneAction`] holds the vote actions on the Hyperlane
//!   infrastructure: `addHyperlaneChain` and `removeHyperlaneChain`.
//! - Every state-changing call to an ISM or to the ICA router goes through
//!   the [`Owner`] trait. The precompile implements it, so `msg.sender` of
//!   each call is `0x…EE14`. This module does not depend on the precompile.

pub mod abi;
pub mod actions;
pub mod errors;
pub mod lifecycle;
pub mod owner;
pub mod schema;
pub mod sol_ext;

mod dispatch;
mod domains;
mod ism_sync;
mod liveness;
mod runtime;

pub use actions::HyperlaneAction;
pub use dispatch::dispatch;
pub use liveness::{
    checkpoint_digest, SignedCheckpoint, GRACE_BLOCKS, LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use owner::{Owner, RemoteCall};
pub use runtime::{consensus_threshold, validate_validators, ControllerBootstrap};
pub use schema::HyperlaneControllerContract;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

#[cfg(test)]
mod tests;
