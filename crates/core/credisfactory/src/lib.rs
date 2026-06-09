//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle
//! on top of the shielded gratis pool:
//!
//! - `requestCredis` verifies a pledge-commitment spend proof through
//!   [`outbe_gratispool`], persists the per-position `(denom_id,
//!   reclaim_commitment)` pair, opens an [`outbe_credis`] position, and
//!   delivers the stablecoin loan via the vault sub-call.
//! - `anadosis` advances the position's installment schedule. When the
//!   position completes the runtime re-inserts the stored reclaim
//!   commitment back into the gratispool so the holder of the reclaim
//!   secret can later `unpledgeGratis(args, destination)`.

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
