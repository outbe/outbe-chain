//! NodFactory: encrypted Nod issuance, settlement and mining, and the Nod call
//! and close sweeps. Nod data lives in the Nod entity store at
//! [`outbe_system_common::addresses::NOD_ADDRESS`]; NodFactory carries no storage of its own.

pub mod api;
pub mod call;
pub(crate) mod call_state;
pub mod close;
pub mod errors;
mod forfeit;
pub mod hooks;
pub mod offer;
pub mod precompile;
pub mod runtime;
pub mod sol_ext;

pub use hooks::NodFactoryModule;

#[cfg(test)]
mod tests;
