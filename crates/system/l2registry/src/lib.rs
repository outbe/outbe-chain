//! `L2Registry` - storage-backed registry of L2 networks (`0x...EE0E`).
//!
//! Records each network's L1 operator address and BLS MinSig committee group
//! key, keyed by non-zero `chain_id`. Public keys use EIP-2537 G2 encoding
//! (256 bytes) at all API boundaries; existing compressed storage stays unchanged.
//! The validator [`vote_target::L2RegistryVoteTarget`] applies registration. The
//! public precompile exposes registry views and operator-authorized key rotation
//! and removal. The operator may be an EOA or a contract. Authorization uses the
//! immediate caller, not the transaction origin.
//! An empty registration key or 256 zero bytes selects the registered inbox's
//! `groupPubKey()` getter. The inbox may differ from the owner. Old records
//! default to the owner. Registry views and Tribute verification resolve that
//! EIP-2537 G2 key via STATICCALL on every use, validate it, and never cache it.
//!
//! The cross-module surface ([`api`]) verifies the selected chain's BLS signature
//! over `zkMerkleRoot` for `TributeFactory.offerTribute`, independent of the caller.
//!
//! [`api::l2_circuits`] uses explicit deployment bindings outside Devnet.
//! Devnet may use a frozen development binding for unbound L2s without
//! changing signature or proof verification.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod public_key;
pub mod schema;
pub mod vote_target;

mod runtime;

pub use schema::{L2NetworkRecord, L2RegistryContract, BLS_PUBLIC_KEY_LEN};
pub use vote_target::{L2RegistryVotePayloadV1, L2RegistryVoteTarget};

#[cfg(test)]
mod tests;
