//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle
//! on top of the shielded gratis pool:
//!
//! - `requestCredis` verifies a pledge-commitment spend proof through
//!   [`outbe_gratispool`], persists the position's `denom_id`, opens an
//!   [`outbe_credis`] position, and delivers the stablecoin loan via the
//!   vault sub-call.
//! - `anadosis` advances the position's installment schedule and inserts the
//!   caller-supplied reclaim commitment for that installment into the
//!   gratispool (at the anadosis denomination) so the holder of the reclaim
//!   secret can `unpledgeGratis(args, destination)` one installment's share
//!   immediately.

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
