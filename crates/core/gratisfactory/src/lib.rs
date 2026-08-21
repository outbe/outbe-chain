//! Gratisfactory precompile (`0x2003`). Thin orchestration layer on top of the
//! confidential Gratis token (`outbe_gratis`) and the Fidelity ledger.

pub mod api;
pub mod constants;
pub mod errors;
pub mod lifecycle;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

#[cfg(test)]
mod tests;
