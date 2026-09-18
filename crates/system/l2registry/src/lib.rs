//! `L2Registry` - storage-backed registry of L2 networks (`0x...EE0E`).
//!
//! Records each network's L1 operator address and BLS MinSig committee group
//! key (compressed G2, 96 bytes), keyed by non-zero `chain_id`. Registration is
//! applied by the validator [`vote_target::L2RegistryVoteTarget`]; the public
//! precompile exposes registry views and owner-authorized removal.
//!
//! The cross-module surface ([`api`]) verifies the selected chain's BLS signature
//! over `zkMerkleRoot` for `TributeFactory.offerTribute`, independent of the caller.
//!
//! [`api::l2_keys`] uses explicit deployment bindings outside Devnet.
//! Devnet may use a frozen development binding for unbound L2s without
//! changing signature or proof verification.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod schema;
pub mod vote_target;

mod runtime;

pub use schema::{L2NetworkRecord, L2RegistryContract, BLS_PUBLIC_KEY_LEN};
pub use vote_target::{L2RegistryVotePayloadV1, L2RegistryVoteTarget};

#[cfg(test)]
mod tests;
