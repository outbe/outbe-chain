//! `crosschainbridge` - the owner of every multi-chain contract (`0x...EE15`).
//!
//! The precompile at the owner address is in this crate. It owns the ERC-7786
//! hub, the gateway adapters, the token bridges and the Hyperlane core
//! contracts: on Outbe directly, on every other chain through its Interchain
//! Account (OIP-00369).
//!
//! - `runtime` runs the owner calls (`call_local`, `call_remote`) and pays
//!   remote dispatch fees from the address balance. `fund` adds value
//!   to that balance.
//! - `callLocal` and `callRemote` let a precompile in [`constants::ALLOWED_CALLERS`]
//!   make the owner call a contract: the HyperlaneController (`0x...EE14`) for
//!   its ISM rotation, bootstrap and Hyperlane chain votes, and Update for an
//!   approved `upgradeContracts` proposal. An upgrade or a configuration change
//!   of our contracts is an `upgradeContracts` vote.
//!
//! It has no vote target and no storage of its own.

pub mod constants;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

#[cfg(test)]
mod tests;
