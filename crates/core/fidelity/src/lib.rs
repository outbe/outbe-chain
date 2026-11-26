//! Fidelity (RCFI) precompile (`0x100C`).
//!
//! The per-owner cohort ledger (the Gratis movement history) is **encrypted at
//! rest**: the TEE enclave is the only party that decrypts it. RCFI/league are
//! never computed on-chain. The enclave produces them through:
//! - cohort ops ([`api::cohort_in`]/[`api::cohort_out`])
//! - the per-WWD league snapshot ([`api::snapshot_leagues`]/[`api::league`])
//! - owner-authorized signed queries (the [`precompile`] eth_call path)
//!
//! Every enclave interaction carries the same determinism (canonical-hash recheck)
//! and attestation guarantees as the confidential Gratis path.
//!
//! Module layout:
//! - [`api`] - cross-crate surface (cohort hooks + league lookups).
//! - [`precompile`] - inbound ABI (signed-auth index queries + plaintext
//!   metadata). No cohort mutations go through the ABI.
//! - [`enclave_client`] - host caller for witness and session calls, plus the
//!   in-process test stand-in.
//! - [`engine`] - cohort mutations, league snapshots, and authorized queries.
//! - [`protocol`] - request and result types, canonical hashes, attestation preimages.
//! - `runtime` / `state` - orchestration and cohort-blob CRUD (crate-private).
//! - `schema` - encrypted storage layout for the [`FidelityContract`] facade.
//! - [`math`] - re-export of the shared fixed-point RCFI arithmetic.

#[cfg(all(feature = "test-enclave", feature = "production-enclave"))]
compile_error!(
    "outbe-fidelity: `test-enclave` selects an in-process stand-in that skips \
     attestation, and it has been unified into a production build. Some \
     dependency edge enables it outside [dev-dependencies] — cargo features \
     are additive, so one such edge anywhere in the graph is enough."
);

pub mod api;
pub mod attest;
pub mod cipher;
pub mod cohort;
pub mod decrypt;
pub mod enclave_client;
pub mod engine;
pub mod errors;
pub mod precompile;
pub mod protocol;
pub mod schema;
pub mod witness;

pub(crate) mod runtime;
pub(crate) mod state;

/// Shared fixed-point RCFI decay math. This crate re-exports it so existing
/// `crate::math::*` paths keep working after the arithmetic moved to the leaf
/// crate `outbe-fidelitymath`. The enclave engine also uses that crate.
pub use outbe_fidelitymath as math;

pub use attest::{
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation, FIDELITY_ATTESTATION,
};
pub use cipher::{read_blob, write_blob, CohortTransition};
pub use decrypt::decrypt_fidelity_cohorts;
pub use engine::{
    apply_cohort_op, apply_cohort_section, derive_fidelity_state_key, query_index, read_state,
    snapshot_leagues,
};
pub use errors::{CohortError, EngineError};
pub use outbe_fidelitymath::{MAX_LEAGUE, MIN_LEAGUE};
pub use protocol::{
    eip191_hash, fidelity_cohort_attestation_preimage, fidelity_cohort_canonical_hash,
    fidelity_query_attestation_preimage, fidelity_query_auth_message,
    fidelity_query_canonical_hash, fidelity_snapshot_attestation_preimage,
    fidelity_snapshot_canonical_hash, FidelityCohortOp, FidelityCohortRequest,
    FidelityCohortResult, FidelityLeagueEntry, FidelityOpOutcome, FidelityOpSection,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotEntry, FidelitySnapshotRequest,
};
pub use schema::FidelityContract;
pub use witness::{FidelityLeagueSnapshot, FidelityQueryRejection};

#[cfg(test)]
mod tests;
