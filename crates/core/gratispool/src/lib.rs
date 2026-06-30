//! Shielded Gratis pool precompile (`0x2004`).
//!
//! Tornado-style commitment+nullifier+ZK pool scoped to Gratis pledges. Two
//! deposit paths (user pledge, credisfactory reclaim insert) and two spend
//! paths (`requestCredis` via credisfactory, `unpledgeGratis` direct) share a
//! single per-denomination Merkle tree, root window, and global nullifier
//! set.
//!
//! Crypto reuses the `outbe-poseidon` + `outbe-zk-backend` stack
//! (same UltraHonkKeccak verifier the OWNERSHIP circuit already uses); the
//! Barretenberg SRS is initialised at node startup by `outbe_zkproof::init_crs`.
//! The verification key for the commitment-nullifier proof comes from
//! `outbe_zk_canonical::noir::commitment_nullifier_proof::VK_BYTES`; see the
//! `outbe-circuits` repository for the Noir source and freeze workflow.

pub mod api;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod state;

pub mod zkp_utils;

pub mod verifier;

pub use errors::GratisPoolError;
pub use runtime::SpendArgs;
pub use schema::GratisPoolContract;

#[cfg(test)]
mod tests;
