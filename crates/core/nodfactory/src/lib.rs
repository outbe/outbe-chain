//! NodFactory precompile crate.
//!
//! Owns Nod issuance (called from Lysis through [`api::issue_nod`]) and
//! the user-triggered `settleNod` and `mineGratis` ABI methods. Persistent Nod entity state
//! lives in the Nod entity store at [`outbe_primitives::addresses::NOD_ADDRESS`];
//! NodFactory carries no storage of its own.

pub mod api;
pub mod certified;
pub mod certified_read;
pub mod errors;
pub mod materialization;
pub mod precompile;
pub mod runtime;
pub mod sol_ext;

#[cfg(test)]
mod tests;
