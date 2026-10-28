//! NodFactory: encrypted Nod issuance, settlement and mining, and the Nod call
//! and forfeit sweeps. Nod data lives in the Nod entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]; NodFactory carries no storage of its own.

pub mod api;
pub mod called;
mod cleanup;
pub mod errors;
pub mod forfeited;
pub mod hooks;
pub mod precompile;
pub mod runtime;
pub mod sol_ext;
#[cfg(feature = "e2e-test")]
mod test_arming_e2e;

#[cfg(test)]
mod tests;
