use super::*;

impl UpgradeContextV1 {
    pub(super) fn validate(&self) -> Result<()> {
        if !self.has_distinct_manifests()
            || !self.has_activation_policy()
            || !self.has_distinct_directories()
        {
            eyre::bail!("upgrade context is incomplete or self-referential");
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
            .map_err(|e| eyre::eyre!("saved prepare evidence: {e}"))?;
        let intent = evidence.intent();
        let context =
            outbe_tee::dcap_protocol::DcapOnboardingContextV1::decode_canonical(&self.context)
                .map_err(|e| eyre::eyre!("saved prepare context: {e:?}"))?;
        let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&self.calldata)?;
        let node_sig: [u8; 65] = call
            .nodeSignature
            .as_ref()
            .try_into()
            .map_err(|_| eyre::eyre!("saved node signature length"))?;
        let enclave_sig: [u8; 64] = call
            .enclaveSignature
            .as_ref()
            .try_into()
            .map_err(|_| eyre::eyre!("saved enclave signature length"))?;

        const MISMATCH: &str = "saved network upgrade commitments or signatures are inconsistent";
        eyre::ensure!(
            !self.candidate_manifest_hash.is_zero()
                && intent.operation == AttestationOperationV1::PrepareEnclaveUpgrade,
            MISMATCH
        );
        eyre::ensure!(
            call.evidence.as_ref() == self.evidence && self.calldata == call.abi_encode(),
            MISMATCH
        );
        eyre::ensure!(
            intent.verify_node_signature(&node_sig)
                && intent.verify_enclave_signature(&enclave_sig),
            MISMATCH
        );
        eyre::ensure!(
            context.intent_hash
                == intent
                    .intent_hash()
                    .map_err(|e| eyre::eyre!("saved intent: {e}"))?
                && context.chain_id == intent.chain_id
                && context.genesis_hash == intent.genesis_hash,
            MISMATCH
        );
        eyre::ensure!(
            context.node_id_hash
                == intent
                    .node_id
                    .node_id_hash()
                    .map_err(|e| eyre::eyre!("saved node: {e}"))?
                && context.enclave_id == intent.enclave_id
                && context.binding_id == intent.binding_id,
            MISMATCH
        );
        eyre::ensure!(
            context.policy_hash == intent.policy_hash
                && context.recipient_x25519 == intent.recipient_x25519,
            MISMATCH
        );
        eyre::ensure!(
            keccak256(&self.calldata) == self.transaction.calldata_hash
                && keccak256(&self.transaction.raw_transaction)
                    == self.transaction.transaction_hash,
            MISMATCH
        );
        Ok(())
    }
}
