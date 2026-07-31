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
pub mod v1;
#[cfg(feature = "tee-attestation-v1")]
pub mod v1_precompile;

pub use runtime::{TeeBootstrapData, TeeRegistration};
pub use schema::TeeRegistry;
pub use v1::{V1RegistrationOutcome, ValidatorEnclaveBindingV1};

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "tee-attestation-v1"))]
mod v1_tests;
