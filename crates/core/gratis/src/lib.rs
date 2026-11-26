//! Confidential Gratis token precompile (`0x1003`).
//!
//! A non-transferable, mineable/burnable balance ledger. Its per-account balances
//! are **encrypted at rest**. The enclave decrypts them on the write path. The
//! account's view-key holder decrypts them client-side. Every write goes through
//! the enclave:
//!
//! 1. Read the current ciphertext.
//! 2. Apply the op inside the enclave.
//! 3. Store the returned ciphertext verbatim.
//!
//! This flow mirrors the determinism + attestation model of the offer path.
//!
//! Module layout:
//! - [`api`] - cross-crate surface (owner-authorized writes + credis-driven ops +
//!   ciphertext reads). Other crates call `outbe_gratis::api::*`.
//! - [`precompile`] - inbound ABI (metadata + confidential reads + non-transferable
//!   stubs). No writes go through the ABI.
//! - [`enclave_client`] - host caller for `ApplyGratisOp` (determinism + attestation
//!   checks) plus the in-process test enclave.
//! - `runtime` / `state` - orchestration and ledger CRUD (crate-private).
//! - `schema` - encrypted storage layout for the [`Gratis`] facade.

#[cfg(all(feature = "test-enclave", feature = "production-enclave"))]
compile_error!(
    "outbe-gratis: `test-enclave` selects an in-process stand-in that skips \
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
pub mod nod_mine;
pub mod precompile;
pub mod protocol;
pub mod schema;
pub mod witness;

pub(crate) mod metadata;
pub(crate) mod resident;
pub(crate) mod runtime;
pub(crate) mod state;

pub use attest::{verify_gratis_op_attestation, GRATIS_OP_ATTESTATION};
pub use decrypt::decrypt_gratis_balance;
pub use engine::{
    apply_op, decrypt_balance, decrypt_pledged, derive_account_keys, derive_gratis_state_key,
    derive_view_key, modify_mac, ModifyOperation,
};
pub use protocol::{
    derive_gratis_keys_message, gratis_op_attestation_preimage, gratis_op_canonical_hash, GratisOp,
    GratisOpRequest, GratisOpResult, GratisOpStatus, ModifyAuth, SealedPledge,
};
pub use schema::Gratis;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
