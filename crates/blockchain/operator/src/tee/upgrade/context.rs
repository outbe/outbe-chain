use super::*;

impl UpgradeContextV1 {
    pub(super) fn validate(&self) -> Result<()> {
        if !self.has_distinct_manifests()
            || !self.has_activation_policy()
            || !self.has_distinct_directories()
        {
            return Err(UpgradeError::InvalidUpgradeContext.into());
        }
        Ok(())
    }

    fn has_distinct_manifests(&self) -> bool {
        !self.predecessor_manifest_hash.is_zero()
            && !self.candidate_manifest_hash.is_zero()
            && self.predecessor_manifest_hash != self.candidate_manifest_hash
    }
    fn has_activation_policy(&self) -> bool {
        !self.successor_policy_hash.is_zero() && self.activation_height != 0
    }
    fn has_distinct_directories(&self) -> bool {
        !self.active_tee_dir.as_os_str().is_empty()
            && !self.candidate_tee_dir.as_os_str().is_empty()
            && self.active_tee_dir != self.candidate_tee_dir
    }
}

impl NetworkUpgradeSubmissionV1 {
    pub(super) fn validate(&self) -> Result<()> {
        let evidence = AttestationEvidenceV1::decode_canonical(&self.evidence)
            .map_err(UpgradeError::SavedPrepareEvidence)?;
        let intent = evidence.intent();
        let context =
            outbe_tee::dcap_protocol::DcapOnboardingContextV1::decode_canonical(&self.context)
                .map_err(UpgradeError::SavedPrepareContext)?;
        let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&self.calldata)?;
        let node_sig: [u8; 65] = call
            .nodeSignature
            .as_ref()
            .try_into()
            .map_err(|_| UpgradeError::SavedNodeSignatureLength)?;
        let enclave_sig: [u8; 64] = call
            .enclaveSignature
            .as_ref()
            .try_into()
            .map_err(|_| UpgradeError::SavedEnclaveSignatureLength)?;

        let intent_hash = intent
            .intent_hash()
            .map_err(UpgradeError::SavedIntentHash)?;
        let node_id_hash = intent
            .node_id
            .node_id_hash()
            .map_err(UpgradeError::SavedNodeHash)?;
        let call_matches = !self.candidate_manifest_hash.is_zero()
            && intent.operation == AttestationOperationV1::PrepareEnclaveUpgrade
            && call.evidence.as_ref() == self.evidence
            && self.calldata == call.abi_encode();
        let signatures_match = intent.verify_node_signature(&node_sig)
            && intent.verify_enclave_signature(&enclave_sig)
            && keccak256(&self.calldata) == self.transaction.calldata_hash
            && keccak256(&self.transaction.raw_transaction) == self.transaction.transaction_hash;
        let context_matches = context_matches_intent(&context, intent, intent_hash, node_id_hash);
        if !(call_matches && signatures_match && context_matches) {
            return Err(UpgradeError::SavedNetworkUpgradeMismatch.into());
        }
        Ok(())
    }
}

fn context_matches_intent(
    context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    intent_hash: B256,
    node_id_hash: B256,
) -> bool {
    let header_matches = context.intent_hash == intent_hash
        && context.chain_id == intent.chain_id
        && context.genesis_hash == intent.genesis_hash;
    let identity_matches = context.node_id_hash == node_id_hash
        && context.enclave_id == intent.enclave_id
        && context.binding_id == intent.binding_id;
    let target_matches = context.policy_hash == intent.policy_hash
        && context.recipient_x25519 == intent.recipient_x25519;
    header_matches && identity_matches && target_matches
}
