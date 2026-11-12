//! Neutral code-registry identities, receipts, and patch-vote subjects.

use alloy_primitives::{Bytes, FixedBytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_macros::storage_record;
use serde::{Deserialize, Serialize};

use crate::error::{PrecompileError, Result};
use crate::radicle_publication::PublicationBody;

/// Radicle repository, patch, or revision identifier.
pub type RepoId = FixedBytes<20>;

/// Approved decision awaits its separate off-chain execution.
pub const PATCH_EXECUTION_PENDING: u8 = 1;
/// The approved patch has been reviewed.
pub const PATCH_EXECUTION_REVIEWED: u8 = 2;
/// The approved patch has been merged.
pub const PATCH_EXECUTION_MERGED: u8 = 3;

/// Maximum accepted canonical JSON size of a patch vote subject.
pub const MAX_PATCH_VOTE_PAYLOAD_BYTES: usize = 512;

// Signatures match `ICodeRegistry` so revert bytes stay identical.
mod revert {
    alloy_sol_types::sol! {
        error NonZeroStorageKeyTail();
        error InvalidPatchPayload();
    }
}

fn non_zero_storage_key_tail() -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(revert::NonZeroStorageKeyTail {}.abi_encode()))
}

fn invalid_patch_payload() -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(revert::InvalidPatchPayload {}.abi_encode()))
}

/// Left-aligned storage key for a 20-byte repository id.
#[must_use]
pub fn repo_id_storage_key(repo_id: RepoId) -> B256 {
    let mut word = [0_u8; 32];
    word[..20].copy_from_slice(repo_id.as_slice());
    B256::from(word)
}

/// Recover a repository id from a left-aligned storage key.
pub fn repo_id_from_storage_key(key: B256) -> Result<RepoId> {
    if key.as_slice()[20..] != [0_u8; 12] {
        return Err(non_zero_storage_key_tail());
    }
    Ok(RepoId::from_slice(&key.as_slice()[..20]))
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[storage_record(exists_field = accepted_height)]
pub struct PublicationReceiptRecord {
    #[key]
    pub sequence: u64,
    #[attribute(order = 0)]
    pub body: Vec<u8>,
    #[attribute(order = 1)]
    pub accepted_height: u64,
    #[attribute(order = 2)]
    pub receipt_hash: B256,
}

/// Immutable patch decision. Execution and merge progress are stored separately.
#[derive(Clone, Debug, PartialEq, Eq)]
#[storage_record(exists_field = approved_height)]
pub struct PatchApprovalRecord {
    #[key]
    pub proposal_id: U256,
    #[attribute(order = 0)]
    pub payload: String,
    #[attribute(order = 1)]
    pub approved_height: u64,
    #[attribute(order = 2)]
    pub approved_timestamp: u64,
}

/// Accepted publication checkpoint. Git I/O stays outside this record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationReceipt {
    pub body: PublicationBody,
    pub accepted_height: u64,
    pub receipt_hash: B256,
}

/// Exact, bounded Vote subject. Git existence and signatures are checked later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchVotePayloadV1 {
    version: u8,
    pub repo_id: RepoId,
    pub patch_id: RepoId,
    pub revision_id: RepoId,
    action: String,
    target_branch: String,
    pub expected_target_head: RepoId,
}

impl PatchVotePayloadV1 {
    /// Canonical accept subject for `main`.
    pub fn accept(repo: RepoId, patch: RepoId, revision: RepoId, target: RepoId) -> Self {
        Self {
            version: 1,
            repo_id: repo,
            patch_id: patch,
            revision_id: revision,
            action: "accept".into(),
            target_branch: "main".into(),
            expected_target_head: target,
        }
    }

    /// Decode one canonical JSON subject.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() > MAX_PATCH_VOTE_PAYLOAD_BYTES {
            return Err(invalid_patch_payload());
        }
        let decoded: Self = serde_json::from_slice(payload).map_err(|_| invalid_patch_payload())?;
        if decoded.encode()? != payload {
            return Err(invalid_patch_payload());
        }
        Ok(decoded)
    }

    /// Encode the subject as canonical JSON.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != 1 || self.action != "accept" || self.target_branch != "main" {
            return Err(invalid_patch_payload());
        }
        if self.repo_id.is_zero() || self.patch_id.is_zero() {
            return Err(invalid_patch_payload());
        }
        if self.revision_id.is_zero() || self.expected_target_head.is_zero() {
            return Err(invalid_patch_payload());
        }
        serde_json::to_vec(self).map_err(|_| invalid_patch_payload())
    }
}
