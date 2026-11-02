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
//! - This crate makes no call to another contract. A write to an ISM or the
//!   ICA router is returned as [`Calls`], and `crosschaingov` runs it from the
//!   owner address.

pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod schema;
pub mod sol_ext;

mod calls;
mod ism;
mod liveness;
mod runtime;

pub use calls::{Calls, RemoteCall};
pub use liveness::{
    checkpoint_digest, SignedCheckpoint, GRACE_BLOCKS, LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use runtime::{consensus_threshold, validate_validators, ControllerBootstrap};
pub use schema::HyperlaneControllerContract;
