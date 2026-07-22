//! `L2Registry` — storage-backed registry of L2 networks (`0x…EE0E`).
//!
//! Records registered L2 networks keyed by `chain_id`: the L1 operator address
//! that submits on behalf of the network, the network's BLS MinPk public key
//! (48 bytes, the same variant used for validator consensus keys), and a
//! per-network `zk_enabled` flag. All mutating methods are permissionless by
//! design.
//!
//! The cross-module surface ([`api`]) verifies the BLS signature carried in
//! `TributeFactory.offerTribute` over `zkMerkleRoot` against the caller's
//! registered network key when that network has ZK verification enabled.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod schema;

mod runtime;

pub use schema::{L2NetworkRecord, L2RegistryContract, BLS_PUBLIC_KEY_LEN};

#[cfg(test)]
mod tests;
