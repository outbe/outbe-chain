//! Confidential Gratis token precompile (`0x1003`).
//!
//! Accounts and collateral allocations live in the enclave's resident ledger.
//! Chain state contains a globally ordered encrypted journal and public supply
//! aggregates. Private queries return receipts encrypted to the owner's view key.
//! [`api`] orchestrates commands; [`enclave_client`] supplies the test backend.

pub mod api;
pub mod enclave_client;
pub mod precompile;
pub mod schema;

pub(crate) mod runtime;
pub(crate) mod state;

pub use schema::Gratis;

#[cfg(test)]
mod tests;
