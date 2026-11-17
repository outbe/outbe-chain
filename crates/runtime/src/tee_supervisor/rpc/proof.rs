//! Same proof records as CLI admission: epoch-zero anchor, certified transitions,
//! exact finalized header and Registry MPT openings. The enclave verifies BLS/MPT.
use super::*;
use alloy_consensus::private::alloy_rlp;
use alloy_sol_types::SolEvent;
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use outbe_tee::{dcap_protocol::DcapOnboardingArtifactV1, finalized_admission::*};

pub(super) fn invalid(reason: ProofFailure) -> RpcLifecycleError {
    RpcLifecycleError::InvalidProof(reason)
}
impl RpcLifecyclePort {
    pub(super) fn proof_checkpoint(
        &self,
        at: LifecycleCheckpointV1,
        proof: &UpgradeKeyProofV1,
    ) -> Result<(), RpcLifecycleError> {
        let witness = FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission)?;
        let header: alloy_consensus::Header =
            alloy_rlp::decode_exact(&witness.admission.header).map_err(RpcLifecycleError::Rlp)?;
        if header.hash_slow() != at.hash
            || header.number != at.height
            || header.state_root != at.state_root
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(())
    }
    pub(super) fn context_identity(
        &self,
        context: &DcapOnboardingContextV1,
    ) -> Result<(), RpcLifecycleError> {
        if context.chain_id != U256::from(self.chain_id).to_be_bytes::<32>()
            || context.genesis_hash != self.genesis_hash
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(())
    }
    pub(crate) async fn upgrade_proof(
        &self,
        at: LifecycleCheckpointV1,
        context: &DcapOnboardingContextV1,
    ) -> Result<UpgradeKeyProofV1, RpcLifecycleError> {
        self.context_identity(context)?;
        self.assemble(at, &upgrade_registry_slots_v1(context)).await
    }
    async fn assemble(
        &self,
        at: LifecycleCheckpointV1,
        slots: &[B256],
    ) -> Result<UpgradeKeyProofV1, RpcLifecycleError> {
        self.check(at).await?;
        if at.height == 0 {
            return Err(invalid(ProofFailure::MissingAdmission));
        }
        let opening: Value = self
            .client
            .request(
                "eth_getProof",
                rpc_params![TEE_REGISTRY_ADDRESS, slots, Self::tag(at)],
            )
            .await?;
        let history = self.admission_history(at).await?;
        let (registry_account, registry_storage) = super::opening::decode_opening(&opening, slots)?;
        let (anchor, transitions, admission) = history.finish()?;
        let witness = FinalizedAdmissionWitnessV1 {
            admission,
            registry_account,
            registry_storage,
        };
        let proof = UpgradeKeyProofV1 {
            anchor_outcome: anchor,
            committee_transitions: transitions,
            admission: witness.encode_canonical()?.into(),
        };
        proof.validate()?;
        self.check(at).await?;
        Ok(proof)
    }
    pub(super) async fn onboarding(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
    ) -> Result<LifecycleOnboardingProofV1, RpcLifecycleError> {
        self.check(at).await?;
        let mut found = None;
        // Bound individual log responses while discovering the intent's finalized artifact.
        for start in (0..=at.height).step_by(2000) {
            let end = start.saturating_add(1999).min(at.height);
            let logs:Vec<Value> = self.client.request("eth_getLogs",rpc_params![json!({"address":TEE_REGISTRY_ADDRESS,"topics":[ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE_HASH],"fromBlock":format!("0x{start:x}"),"toBlock":format!("0x{end:x}")})]).await?;
            for log in logs {
                let topics: Vec<B256> = serde_json::from_value(
                    log.get("topics")
                        .cloned()
                        .ok_or(RpcLifecycleError::Missing("topics"))?,
                )?;
                let event = ITeeRegistryV1::OfferKeySealedForRegistryV1::decode_raw_log_validate(
                    topics,
                    &bytes(&log, "data")?,
                )?;
                let artifact = DcapOnboardingArtifactV1::decode_canonical(&event.sealedOfferKey)
                    .map_err(RpcLifecycleError::Dcap)?;
                if artifact.context.intent_hash != intent {
                    continue;
                }
                self.context_identity(&artifact.context)?;
                let address: Address = serde_json::from_value(
                    log.get("address")
                        .cloned()
                        .ok_or(RpcLifecycleError::Missing("address"))?,
                )?;
                if address != TEE_REGISTRY_ADDRESS
                    || event.nodeIdHash != artifact.context.node_id_hash
                    || log.get("removed").and_then(Value::as_bool) != Some(false)
                {
                    return Err(invalid(ProofFailure::ArtifactMismatch));
                }
                let receipt = self
                    .receipt(at, hash(&log, "transactionHash")?)
                    .await?
                    .ok_or_else(|| invalid(ProofFailure::InvalidReceipt))?;
                if !receipt.success
                    || receipt.block_height != number(&log, "blockNumber")?
                    || receipt.block_hash != hash(&log, "blockHash")?
                {
                    return Err(invalid(ProofFailure::InvalidReceipt));
                }
                if found.is_some() {
                    return Err(invalid(ProofFailure::AmbiguousArtifact));
                }
                found = Some((event.sealedOfferKey, artifact.context));
            }
        }
        let (artifact, context) = found.ok_or_else(|| invalid(ProofFailure::MissingArtifact))?;
        let proof = self
            .assemble(at, &onboarding_registry_slots_v1(&context))
            .await?;
        Ok(LifecycleOnboardingProofV1 { artifact, proof })
    }
}
