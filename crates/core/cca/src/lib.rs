//! Credis Card Agent registry precompile (`0x1019`).
//!
//! A CCA originates Credis positions for card owners. The program pays it from
//! the daily CCA emission sink for originations it brings in (§8.3) and
//! penalizes it when a position it originated is voided (§8.4). Registration is
//! permissionless (§8.1).
//!
//! What is **not** here: the COEN bond. §8.1/§8.2 price identity reset with a
//! dynamic bond requirement, delegated senior layers, per-layer unbonding
//! cooldowns and an exit haircut — a subsystem of its own. Until it lands,
//! registration is free, so nothing stops an agent from walking away from a
//! penalized multiplier and re-registering fresh. The accountability arithmetic
//! below is complete; its economic teeth are not.
//!
//! The reward distribution lives next door: this crate owns the units an agent
//! earned each day and the multiplier that weights them, and exposes the product
//! of the two through [`api::day_reward_weights`]. Splitting the pool on those
//! weights — the 32% cap, the redistribution, the residue to Metadosis — is
//! `outbe_agentreward`'s `PoolKind::Cca`, which also calls
//! [`api::settle_day`] once a day is paid.

pub mod api;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub(crate) mod state;

pub use errors::CcaError;
pub use schema::{CcaContract, CcaRecord, CcaState};

#[cfg(test)]
mod tests;
