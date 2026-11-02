//! `CrosschainGov` - the governance owner of every multi-chain contract
//! (`0x...EE14`).
//!
//! The precompile owns the Hyperlane core contracts, the ERC-7786 hub, its
//! gateway adapters, the token bridges and the intent contracts: on Outbe
//! directly, on every other chain through its Interchain Account (ICA).
//!
//! - `applyApproved`, `fund` and `remoteAccount` run here. Every other
//!   selector goes to `outbe-hyperlanecontroller` without logic of its own.
//! - [`owner`] holds the owner calls `call_local` and `call_remote`. The
//!   precompile implements `outbe_hyperlanecontroller::Owner` with them, so
//!   `msg.sender` of every owner call is `0x...EE14`.
//! - [`CrosschainVoteTarget`] is the one vote target of `0x...EE14`. It reads
//!   the `operation` of a payload. `addHyperlaneChain` and
//!   `removeHyperlaneChain` go to `outbe-hyperlanecontroller`. Every other
//!   operation goes to [`actions`].
//! - The precompile pays remote dispatch fees (IGP quote) from its own
//!   balance. `fund` adds value to that balance.

pub mod actions;
pub mod errors;
pub mod owner;
pub mod precompile;
pub mod schema;
pub mod sol_ext;

mod vote_target;

pub use schema::CrosschainGov;
pub use vote_target::CrosschainVoteTarget;

#[cfg(test)]
mod tests;
