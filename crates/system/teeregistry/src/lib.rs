//! `TeeRegistry` - storage-backed KV precompile (`0x...EE0A`).
//!
//! Records role-neutral V1 NodeHost enclave bindings and the global
//! `tribute_offer_public_key`. The OST3 block-1 system transaction writes this
//! key once. [`v1_precompile`] is the only public ABI. There is no
//! caller-authorized registration dispatcher or fallback admission path.

pub mod bootstrap;
pub mod errors;
pub mod runtime;
pub mod schema;
pub mod upgrade;
pub mod v1;
#[cfg(feature = "tee-attestation-v1")]
pub mod v1_precompile;

pub use bootstrap::{
    run_tee_bootstrap_v1, validate_tee_bootstrap_for_build, TeeBootstrapBuildError,
    TeeBootstrapError, TeeBootstrapModule,
};
pub use runtime::TeeBootstrapData;
pub use schema::TeeRegistry;
pub use v1::{NodeEnclaveBindingV1, RevokeEnclaveBindingConflictV1, V1RegistrationOutcome};

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "tee-attestation-v1"))]
mod v1_tests;
