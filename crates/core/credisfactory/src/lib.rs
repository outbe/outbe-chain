//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `requestCredis` consumes a confidential Gratis pledge (pledge handle +
//!   spend authorization) via [`outbe_gratis`], opens an [`outbe_credis`]
//!   position bound to the bundle account, persists the pledge linkage, and
//!   delivers the stablecoin loan via the vault sub-call.
//! - `anadosis` advances the position's installment schedule and releases 1/N of
//!   the pledged collateral back to the original pledger's encrypted Gratis
//!   balance.

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
