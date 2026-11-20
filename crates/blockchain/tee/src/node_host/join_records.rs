use alloy_primitives::B256;

/// Exact finalized checkpoint that allows a restarted validator to catch up
/// without trusting its stale local Registry state. This is owner-only local
/// recovery evidence. It grants no consensus membership or voting authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedJoinAdmissionAnchorV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub intent_hash: B256,
    pub finalized_height: u64,
    pub finalized_hash: B256,
    pub finalized_state_root: B256,
    pub finalized_consensus_timestamp: u64,
}

#[cfg(test)]
impl FinalizedJoinAdmissionAnchorV1 {
    /// A disk checkpoint is not key possession. Validate it against this live process.
    /// The caller must still verify resident material before opening authority.
    pub(crate) fn validate_live_manifest_v1(
        &self,
        live: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
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
