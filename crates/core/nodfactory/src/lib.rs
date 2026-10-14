//! NodFactory precompile crate.
//!
//! Owns encrypted Nod materialization and
//! the user-triggered `settleNod` and `mineGratis` ABI methods. Persistent Nod entity state
//! lives in the Nod entity store at [`outbe_primitives::addresses::NOD_ADDRESS`];
//! NodFactory carries no storage of its own.

pub mod api;
pub mod called;
pub mod certified;
pub mod certified_read;
pub mod errors;
pub mod expired;
pub mod hooks;
pub mod materialization;
pub mod precompile;
pub mod runtime;
pub mod sol_ext;
#[cfg(feature = "e2e-test")]
mod test_arming;

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
