//! Confidential Promis token precompile.
//!
//! Per-account balances are encrypted at rest. The resident enclave decrypts
//! them on the write path. The account view key decrypts them client-side.
//! `ApplyPromisOp` is witness tag 2.
//! `outbe_derivePromisKeys` is session-only and is not written to the tape.

#[cfg(all(feature = "test-enclave", feature = "production-enclave"))]
compile_error!(
    "outbe-promis: `test-enclave` selects an in-process stand-in that skips \
     attestation, and it has been unified into a production build. Some \
     dependency edge enables it outside [dev-dependencies] — cargo features \
     are additive, so one such edge anywhere in the graph is enough."
);

pub mod api;
pub mod attest;
pub mod cipher;
pub mod decrypt;
pub mod enclave_client;
pub mod engine;
pub mod errors;
pub mod precompile;
pub mod protocol;
pub mod schema;
pub mod witness;

pub(crate) mod resident;
pub(crate) mod runtime;
pub(crate) mod state;

pub use attest::{verify_promis_op_attestation, PROMIS_OP_ATTESTATION};
pub use decrypt::decrypt_promis_balance;
pub use engine::{
    apply_op, decrypt_balance, derive_account_keys, derive_promis_state_key, derive_view_key,
    modify_mac, ModifyOperation,
};
pub use protocol::{
    derive_promis_keys_message, promis_op_attestation_preimage, promis_op_canonical_hash,
    ModifyAuth, PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus,
};
pub use schema::Promis;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
