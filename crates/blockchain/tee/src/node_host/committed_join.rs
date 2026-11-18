use super::durable_submission::{
    verify_durable_submission, DurableSubmission, DurableSubmissionKind,
};
use super::CommittedJoinSubmissionV1;
use super::FinalizedJoinAdmissionAnchorV1;

use crate::TransportError;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;
use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

pub(super) fn validate_durable_committed_join_submission(
    manifest: &EnclaveInitializationManifestV1,
    submission: &CommittedJoinSubmissionV1,
) -> Result<RegistrationIntentV1, TransportError> {
    verify_durable_submission(
        manifest,
        DurableSubmission {
            evidence: submission.evidence(),
            node_signature: submission.node_signature(),
            enclave_signature: submission.enclave_signature(),
            kind: DurableSubmissionKind::CommittedJoin,
        },
    )
}

impl FinalizedJoinAdmissionAnchorV1 {
    /// A disk checkpoint is not key possession. Validate it against this live process.
    /// The caller must still verify resident material before opening authority.
    #[cfg(test)]
    pub(crate) fn validate_live_manifest_v1(
        &self,
        live: &EnclaveInitializationManifestV1,
    ) -> crate::in_process::Result<()> {
        if self.chain_id != live.chain_id
            || self.genesis_hash != live.genesis_hash
            || self.node_id_hash != live.node_id.node_id_hash()?
            || self.enclave_id != live.enclave_id()?
        {
            return Err(crate::in_process::InProcessError::IdentityMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod live_anchor_tests {
    use super::*;

    #[test]
    fn prior_process_anchor_never_attests_a_fresh_enclave() {
        let active = super::super::identity::process_identity_tests::manifest(9, [4; 32]);
        let fresh = super::super::identity::process_identity_tests::manifest(10, [4; 32]);
        let anchor = FinalizedJoinAdmissionAnchorV1 {
            chain_id: active.chain_id,
            genesis_hash: active.genesis_hash,
            node_id_hash: active.node_id.node_id_hash().unwrap(),
            enclave_id: active.enclave_id().unwrap(),
            intent_hash: B256::repeat_byte(1),
            finalized_height: 91,
            finalized_hash: B256::repeat_byte(2),
            finalized_state_root: B256::repeat_byte(3),
            finalized_consensus_timestamp: 19000,
        };
        anchor.validate_live_manifest_v1(&active).unwrap();
        assert!(matches!(
            anchor.validate_live_manifest_v1(&fresh),
            Err(crate::in_process::InProcessError::IdentityMismatch)
        ));
    }
}
