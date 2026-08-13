//! Gratisfactory precompile (`0x2003`). Thin orchestration layer on top of the
//! confidential Gratis token (`outbe_gratis`) and the Fidelity ledger.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::GratisFactoryContract;

#[cfg(test)]
mod tests;
