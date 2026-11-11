//! Exact, bounded Vote subject; Git existence and signatures are checked after finality.

pub use outbe_primitives::code_registry::{PatchVotePayloadV1, MAX_PATCH_VOTE_PAYLOAD_BYTES};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RepoId;

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
