//! Gratis token precompile (`0x1003`).
//!
//! A non-transferable, mineable/burnable balance ledger with a per-account
//! pledge escrow held at `CREDIS_ADDRESS`. Module layout follows the standard
//! split:
//!
//! - [`api`] — the cross-crate surface; other crates call
//!   `outbe_gratis::api::{mine, burn, pledge, unpledge, balance_of, …}` rather
//!   than constructing the [`Gratis`] facade directly.
//! - [`precompile`] — inbound ABI decode/dispatch/encode for the `IGratis`
//!   interface.
//! - `runtime` — mint/burn/pledge/unpledge business logic (crate-private).
//! - `state` — ledger reads and the internal balance-move transition
//!   (crate-private).
//! - `schema` — storage layout for the [`Gratis`] facade.

pub mod api;
pub mod precompile;
pub mod schema;

pub(crate) mod runtime;
pub(crate) mod state;

pub use schema::Gratis;

#[cfg(test)]
mod tests;
