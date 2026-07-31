//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `requestCredis` consumes a confidential Gratis pledge-lock ticket (pledge
//!   handle + spend authorization) via [`outbe_gratis`], opens an [`outbe_credis`]
//!   position bound to the bundle account (storing the pledger EOA), crediting the
//!   collateral into the pledger's own pledged ledger, and delivers the stablecoin
//!   loan via the vault sub-call.
//! - `anadosis` advances the position's installment schedule and releases that
//!   installment's share of collateral from the pledger's pledged ledger back to its
//!   balance.
//! - [`CredisLifecycle`] sweeps expired positions each block, burning the unpaid
//!   collateral (spec §3.6).

pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use lifecycle::CredisLifecycle;
pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
