//! `TeeRegistry` — storage-backed KV precompile (`0x…EE0A`).
//!
//! Records the per-validator TEE registration bundle and the global
//! `tribute_offer_public_key`, written once by the `TeeBootstrap` system
//! transaction (Phase 3b). The public ABI ([`precompile`]) is **read-only** —
//! clients fetch the offer key via `eth_call`; the initial write is performed
//! natively by the system-tx handler through `StorageHandle::contract`, not via
//! the public ABI (see [`runtime::TeeRegistry::write_bootstrap`]).

pub mod precompile;
pub mod runtime;
pub mod schema;

pub use runtime::{TeeBootstrapData, TeeRegistration};
pub use schema::TeeRegistry;

#[cfg(test)]
mod tests;
