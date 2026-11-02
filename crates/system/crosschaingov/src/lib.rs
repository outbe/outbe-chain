//! `crosschaingov` - the owner of every multi-chain contract (`0x...EE15`).
//!
//! The precompile at the owner address is in this crate. It owns the ERC-7786
//! hub, the gateway adapters, the token bridges and the Hyperlane core
//! contracts: on Outbe directly, on every other chain through its Interchain
//! Account (OIP-00369).
//!
//! - `runtime` runs the owner calls (`call_local`, `call_remote`) and pays
//!   remote dispatch fees from the address balance. `fund` adds value
//!   to that balance.
//! - [`CrosschainVoteTarget`] stores the hash of an approved action.
//!   Permissionless `applyApproved` then runs it. Every vote action is in
//!   `vote_target`: the actions on our contracts and on the Hyperlane chains.
//! - `callLocal` and `callRemote` let a precompile in [`precompile::OWNER_CALLERS`]
//!   make the owner call a contract. Today this is the HyperlaneController
//!   (`0x...EE14`), for its ISM rotation and bootstrap.

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

mod vote_target;

pub use vote_target::CrosschainVoteTarget;

#[cfg(test)]
mod tests;
