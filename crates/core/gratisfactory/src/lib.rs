//! Gratisfactory precompile (`0x2003`). Thin orchestration layer on top of the
//! confidential Gratis token (`outbe_gratis`) and the Fidelity ledger. It keeps
//! the Gratis pledged for Credis reservations until Credis uses or the source
//! cancels them.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

#[cfg(test)]
mod tests;
