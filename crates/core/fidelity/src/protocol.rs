//! Fidelity cohort, snapshot, and query protocol types and canonical digests.
//!
//! Field order and serde attributes match the historical enclave postcard and
//! JSON attestation preimages. [`FidelityCohortOp`] is `repr(u8)` so the
//! canonical-hash byte is the variant index.

use alloy_primitives::{Address, B256, U256};

/// Cohort mutation carried inside a Gratis op, or applied on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum FidelityCohortOp {
    /// Acquisition: push a new active cohort of the Gratis op's amount.
    In = 0,
    /// Sale: consume active cohorts LIFO for the Gratis op's amount.
    Out = 1,
    /// Read-only league probe. No cohort mutation and no blob rewrite.
    Probe = 2,
}

/// Co-located Fidelity input riding in a Gratis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpSection {
    /// Cohort mutation, or a read-only probe.
    pub op: FidelityCohortOp,
    /// Block timestamp in seconds.
    pub timestamp: u64,
    /// Chain-wide first qualification day. Zero means none yet.
    pub first_qualified_start: u64,
    /// Current encrypted cohort blob. Empty for a fresh account.
    pub current_blob: Vec<u8>,
}

/// Plaintext receipt of a [`FidelityOpSection`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpOutcome {
    /// Replacement ciphertext. Empty for a probe.
    pub new_blob: Vec<u8>,
    /// UTC day when this account qualified for the first time.
    pub qualified_start_initialized: Option<u64>,
    /// League after the section, in `1..=4096`.
    pub league: u16,
}

/// Inputs for a standalone cohort op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortRequest {
    /// Chain the host believes it is executing.
    pub chain_id: B256,
    /// Account whose cohort blob changes.
    pub account: Address,
    /// Gratis amount of the op. Zero is a no-op at the host.
    pub amount: U256,
    /// Cohort section read from committed storage.
    pub section: FidelityOpSection,
}

/// Public result of a standalone cohort op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortResult {
    /// Plaintext receipt the host persists.
    pub outcome: FidelityOpOutcome,
    /// Keccak of the canonical request bytes.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag over the cohort attestation preimage.
    pub attestation_tag: Vec<u8>,
}

/// One owner's encrypted cohort blob in a snapshot batch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotEntry {
    /// Account whose league is requested.
    pub owner: Address,
    /// Encrypted cohort blob. Empty means a fresh account.
    pub cohort_blob: Vec<u8>,
}

/// Inputs for a league snapshot batch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotRequest {
    /// Evaluation timestamp in seconds.
    pub timestamp: u64,
    /// Chain-wide first qualification day.
    pub first_qualified_start: u64,
    /// Owners, in the order the leagues must come back.
    pub entries: Vec<FidelitySnapshotEntry>,
}

/// One owner's plaintext league and RCFI, in request order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityLeagueEntry {
    /// Account this league belongs to.
    pub owner: Address,
    /// League in `1..=4096`.
    pub league: u16,
    /// RCFI at the snapshot timestamp. Absent historical JSON defaults to zero.
    #[serde(default)]
    pub rcfi: U256,
}

/// Inputs for an owner-authorized index query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryRequest {
    /// Chain the authorization was signed for.
    pub chain_id: B256,
    /// Account that must have signed the authorization.
    pub account: Address,
    /// Encrypted cohort blob.
    pub cohort_blob: Vec<u8>,
    /// Timestamp the RCFI is evaluated at, in seconds.
    pub query_timestamp: u64,
    /// Block timestamp the expiry is checked against, in seconds.
    pub block_timestamp: u64,
    /// Chain-wide first qualification day.
    pub first_qualified_start: u64,
    /// Authorization expiry, unix seconds.
    pub expiry: u64,
    /// 65-byte recoverable secp256k1 signature.
    pub owner_sig: Vec<u8>,
}

/// Plaintext result of an index query. This is the tag-5 witness body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryResult {
    /// RCFI at `query_timestamp`.
    pub rcfi: U256,
    /// Retention efficiency at `query_timestamp`.
    pub efficiency: U256,
    /// League in `1..=4096`.
    pub league: u16,
    /// Keccak of the canonical request bytes.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag over the query attestation preimage.
    pub attestation_tag: Vec<u8>,
}

/// Domain-tagged message an owner personal-signs to authorize an index query.
#[must_use]
pub fn fidelity_query_auth_message(chain_id: B256, account: Address, expiry: u64) -> Vec<u8> {
    let tag: &[u8] = b"outbe/fidelity/query-auth/v1";
    let mut message = Vec::with_capacity(tag.len() + 32 + 20 + 8);
    message.extend_from_slice(tag);
    message.extend_from_slice(chain_id.as_slice());
    message.extend_from_slice(account.as_slice());
    message.extend_from_slice(&expiry.to_be_bytes());
    message
}

/// EIP-191 hash of `message`. Re-exported so host and enclave share one helper.
pub use outbe_core_common::confidential_tee::eip191_hash;

/// Deterministic hash over the canonical inputs of a standalone cohort op.
#[must_use]
pub fn fidelity_cohort_canonical_hash(req: &FidelityCohortRequest) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.push(req.section.op as u8);
    buf.extend_from_slice(&req.section.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.section.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.section.current_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.section.current_blob);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a standalone cohort op.
pub fn fidelity_cohort_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityCohortResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    framed_preimage(
        b"outbe/tee/fidelity-cohort-attestation/v1",
        inputs_canonical_hash,
        &probe,
    )
}

/// Deterministic hash over the canonical inputs of a league snapshot batch.
#[must_use]
pub fn fidelity_snapshot_canonical_hash(req: &FidelitySnapshotRequest) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(&req.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.entries.len() as u32).to_be_bytes());
    for entry in &req.entries {
        buf.extend_from_slice(entry.owner.as_slice());
        buf.extend_from_slice(&(entry.cohort_blob.len() as u32).to_be_bytes());
        buf.extend_from_slice(&entry.cohort_blob);
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a snapshot batch.
///
/// The signed JSON is the league list, not the witness envelope around it.
pub fn fidelity_snapshot_attestation_preimage(
    inputs_canonical_hash: B256,
    leagues: &[FidelityLeagueEntry],
) -> Result<Vec<u8>, serde_json::Error> {
    framed_preimage(
        b"outbe/tee/fidelity-snapshot-attestation/v1",
        inputs_canonical_hash,
        leagues,
    )
}

/// Deterministic hash over the canonical inputs of one index query.
#[must_use]
pub fn fidelity_query_canonical_hash(req: &FidelityQueryRequest) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&(req.cohort_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.cohort_blob);
    buf.extend_from_slice(&req.query_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.block_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&req.expiry.to_be_bytes());
    buf.extend_from_slice(&(req.owner_sig.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.owner_sig);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for an index query.
pub fn fidelity_query_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityQueryResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    framed_preimage(
        b"outbe/tee/fidelity-query-attestation/v1",
        inputs_canonical_hash,
        &probe,
    )
}

fn framed_preimage<T: serde::Serialize + ?Sized>(
    domain: &[u8],
    inputs_canonical_hash: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(value)?;
    let mut buf = Vec::with_capacity(domain.len() + 32 + 4 + json.len());
    buf.extend_from_slice(domain);
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}
