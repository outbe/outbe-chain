//! `HyperlaneController` - the Hyperlane infrastructure of the owner address
//! (`0x...EE14`).
//!
//! The owner address owns every Hyperlane core contract on Outbe (Mailbox,
//! ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
//! StorageMessageIdMultisigIsm). Through its Interchain Account, it also owns
//! the same contracts on remote chains. This crate keeps the Hyperlane logic of
//! that address. The `crosschaingov` crate serves the address and dispatches
//! its selectors here.
//!
//! - It holds no validator set of its own. The ISMs are the source of truth.
//! - `sync` rotates validators through `set_validators_and_threshold`.
//!   Begin-block liveness calls the same rotation.
//! - `actions` holds the vote actions on the Hyperlane chains.
//! - Every write to an ISM or the ICA router goes through [`OwnerCalls`],
//!   which `crosschaingov` implements.

pub mod actions;
pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod schema;
pub mod sol_ext;

mod ism_sync;
mod liveness;
mod owner_calls;
mod remote;
mod runtime;

pub use liveness::{
    checkpoint_digest, SignedCheckpoint, GRACE_BLOCKS, LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use owner_calls::{OwnerCalls, RemoteCall};
pub use runtime::{consensus_threshold, validate_validators, ControllerBootstrap};
pub use schema::HyperlaneControllerContract;
