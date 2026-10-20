//! Exact, bounded Vote subject; Git existence and signatures are checked after finality.

use outbe_primitives::error::Result;
use serde::{Deserialize, Serialize};

use crate::{errors::CodeRegistryError, RepoId};

pub const MAX_PATCH_VOTE_PAYLOAD_BYTES: usize = 512;

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

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() > MAX_PATCH_VOTE_PAYLOAD_BYTES {
            return Err(CodeRegistryError::InvalidPatchPayload.into());
        }
        let decoded: Self =
            serde_json::from_slice(payload).map_err(|_| CodeRegistryError::InvalidPatchPayload)?;
        // One byte representation excludes alternate field orders, escapes and OID encodings.
        if decoded.encode()? != payload {
            return Err(CodeRegistryError::InvalidPatchPayload.into());
        }
        Ok(decoded)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.version != 1 || self.action != "accept" || self.target_branch != "main" {
            return Err(CodeRegistryError::InvalidPatchPayload.into());
        }
        if self.repo_id.is_zero() || self.patch_id.is_zero() {
            return Err(CodeRegistryError::InvalidPatchPayload.into());
        }
        if self.revision_id.is_zero() || self.expected_target_head.is_zero() {
            return Err(CodeRegistryError::InvalidPatchPayload.into());
        }
        serde_json::to_vec(self).map_err(|_| CodeRegistryError::InvalidPatchPayload.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_subject_roundtrip_and_noncanonical_inputs_are_rejected() {
        let subject = PatchVotePayloadV1::accept(
            RepoId::repeat_byte(0xaa),
            RepoId::repeat_byte(0xbb),
            RepoId::repeat_byte(0xcc),
            RepoId::repeat_byte(0xdd),
        );
        let encoded = String::from_utf8(subject.encode().unwrap()).unwrap();
        assert_eq!(encoded, format!(
            "{{\"version\":1,\"repoId\":\"0x{}\",\"patchId\":\"0x{}\",\"revisionId\":\"0x{}\",\"action\":\"accept\",\"targetBranch\":\"main\",\"expectedTargetHead\":\"0x{}\"}}",
            "aa".repeat(20), "bb".repeat(20), "cc".repeat(20), "dd".repeat(20)
        ));
        assert_eq!(
            PatchVotePayloadV1::decode(encoded.as_bytes()).unwrap(),
            subject
        );
        for bad in [
            format!(" {encoded}"),
            encoded.replace("\"version\":1", "\"version\":1,\"version\":1"),
            encoded.replace("\"version\":1", "\"version\":2"),
            encoded.replace("\"version\":1", "\"extra\":0,\"version\":1"),
            encoded.replace("\"accept\"", "\"merge\""),
            encoded.replace("\"main\"", "\"other\""),
            encoded.replace(&"aa".repeat(20), &"AA".repeat(20)),
            encoded.replace(&"bb".repeat(20), &"00".repeat(20)),
            encoded.replace("\"version\":1", "\"version\":1.0"),
            " ".repeat(MAX_PATCH_VOTE_PAYLOAD_BYTES + 1),
        ] {
            assert!(PatchVotePayloadV1::decode(bad.as_bytes()).is_err(), "{bad}");
        }
        let revised = PatchVotePayloadV1::accept(
            subject.repo_id,
            subject.patch_id,
            RepoId::repeat_byte(0xee),
            subject.expected_target_head,
        );
        assert_ne!(revised.encode().unwrap(), subject.encode().unwrap());
    }
}
