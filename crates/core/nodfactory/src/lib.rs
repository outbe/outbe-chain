//! NodFactory precompile crate.
//!
//! Owns Nod issuance (called from Lysis through [`api::issue_nod`]) and
//! the user-triggered `mineGratis` ABI method. Persistent Nod entity state
//! lives in the Nod entity store at [`outbe_primitives::addresses::NOD_ADDRESS`];
//! NodFactory carries no storage of its own.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod runtime;
mod sol_ext;

#[cfg(test)]
mod tests;
