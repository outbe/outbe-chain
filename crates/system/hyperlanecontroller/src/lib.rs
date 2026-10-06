//! `HyperlaneController` - governance-owned controller of the Hyperlane bridge (`0x...EE14`).
//!
//! The precompile owns every Hyperlane core contract on Outbe (Mailbox,
//! ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
//! StorageMessageIdMultisigIsm). Through its Interchain Account, it also owns
//! the same contracts on remote chains. It holds no validator set of its own.
//! The ISMs are the source of truth. The controller only forwards owner calls.
//!
//! - `initialize`, `fund` and the permissionless `sync` (mirror the active
//!   validator set into every ISM) are the only direct write selectors.
//! - Validator rotation, generic local / remote owner calls and table changes
//!   are methods on [`HyperlaneControllerContract`]. The trigger that runs them
//!   (validator vote or another authority) is not wired yet.
//! - The controller pays remote dispatch fees (IGP quote) from its own
//!   balance. `fund` adds value to that balance.

pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod schema;

mod runtime;
mod sol_ext;

pub use runtime::{
    checkpoint_digest, consensus_threshold, validate_validators, RemoteCall, GRACE_BLOCKS,
    LIVENESS_WINDOW_BLOCKS, MAX_MISSES,
};
pub use schema::HyperlaneControllerContract;

#[cfg(test)]
mod tests;
