//! Witness schemas for the recordable Fidelity responses.
//!
//! Tag 4 is the league snapshot. Tags 5 and 15 are the two query answers.
//! The outer V1 tag selects the schema. Neither body carries a second postcard
//! discriminant. Cohort ops stay off the witness tape (tag 0, session-only).

use alloy_primitives::B256;

use crate::protocol::{FidelityLeagueEntry, FidelityQueryResult};

/// Tag-4 body: leagues, inputs hash, and the Ed25519 tag.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityLeagueSnapshot {
    /// One league per requested owner, in request order.
    pub leagues: Vec<FidelityLeagueEntry>,
    /// Keccak of the canonical snapshot request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag over the league-list attestation preimage.
    pub attestation_tag: Vec<u8>,
}

/// Tag-15 body. The reason is signed consensus output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryRejection {
    /// Keccak of the canonical query request.
    pub inputs_canonical_hash: B256,
    /// Display of the engine error that rejected the query.
    pub reason: String,
    /// Ed25519 tag over `outbe/fidelity/query-rejected/v1`.
    pub attestation_tag: Vec<u8>,
}

outbe_core_common::impl_confidential_witness_schema!(
    FidelityLeagueSnapshot,
    FidelityLeaguesSnapshotted
);

outbe_core_common::impl_confidential_witness_schema!(FidelityQueryResult, FidelityIndexQueried);

outbe_core_common::impl_confidential_witness_schema!(FidelityQueryRejection, FidelityQueryRejected);
