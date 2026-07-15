//! Gratisfactory precompile (`0x2003`). Thin orchestration layer on top of the
//! confidential Gratis token (`outbe_gratis`) and the Fidelity ledger.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod runtime;

#[cfg(test)]
mod tests;
