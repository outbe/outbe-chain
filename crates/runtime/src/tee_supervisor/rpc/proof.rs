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

const LOG_CHUNK: u64 = 2_000;
const MAX_REVERSE_CHUNKS: u32 = 4;
impl RpcLifecyclePort {
    /// `at` is the freshness head. The proof may name an older canonical block.
    pub(super) fn proof_header_within_head(
        &self,
        at: LifecycleCheckpointV1,
        proof: &UpgradeKeyProofV1,
    ) -> Result<alloy_consensus::Header, RpcLifecycleError> {
        let witness = FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission)?;
        let header: alloy_consensus::Header =
            alloy_rlp::decode_exact(&witness.admission.header).map_err(RpcLifecycleError::Rlp)?;
        if header.number > at.height {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        if header.number == at.height
            && (header.hash_slow() != at.hash || header.state_root != at.state_root)
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(header)
    }
    pub(super) async fn confirm_proof_checkpoint(
        &self,
        at: LifecycleCheckpointV1,
        proof: &UpgradeKeyProofV1,
    ) -> Result<(), RpcLifecycleError> {
        let header = self.proof_header_within_head(at, proof)?;
        if header.number == at.height {
            return Ok(());
        }
        let canonical = self.block(json!(format!("0x{:x}", header.number))).await?;
        let proved = self.checkpoint(&canonical)?;
        if proved.height != header.number
            || proved.hash != header.hash_slow()
            || proved.state_root != header.state_root
            || proved.timestamp != header.timestamp
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
    async fn accept_finalized(&self, at: LifecycleCheckpointV1) -> Result<(), RpcLifecycleError> {
        if at.chain_id != self.chain_id || at.genesis_hash != self.genesis_hash {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let chain: String = self.client.request("eth_chainId", rpc_params![]).await?;
        let genesis = self.block(json!("0x0")).await?;
        let canonical = self.block(json!(format!("0x{:x}", at.height))).await?;
        let finalized = self.block(json!("finalized")).await?;
        if quantity(&chain)? != self.chain_id
            || hash(&genesis, "hash")? != self.genesis_hash
            || self.checkpoint(&canonical)? != at
            || number(&finalized, "number")? < at.height
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(())
    }
    async fn state_proof(
        &self,
        target: LifecycleCheckpointV1,
        slots: &[B256],
    ) -> Result<(LifecycleCheckpointV1, Value), RpcLifecycleError> {
        let fresh = self.checkpoint(&self.block(json!("finalized")).await?)?;
        if fresh.height < target.height {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        match self
            .client
            .request(
                "eth_getProof",
                rpc_params![TEE_REGISTRY_ADDRESS, slots, Self::tag(target)],
            )
            .await
        {
            Ok(opening) => Ok((target, opening)),
            Err(error) if proof_window_expired(&error) => {
                let fresh = self.checkpoint(&self.block(json!("finalized")).await?)?;
                // Reth measures the window from best, so a still-finalized H1
                // can already be outside 128 even when H2 - H1 <= 128.
                if fresh.height > target.height {
                    Err(RpcLifecycleError::ProofWindowMoved {
                        proved: target.height,
                        fresh_height: fresh.height,
                        fresh: Box::new(fresh),
                        source: Box::new(error),
                    })
                } else {
                    Err(error.into())
                }
            }
            Err(error) => Err(error.into()),
        }
    }
    async fn assemble(
        &self,
        at: LifecycleCheckpointV1,
        slots: &[B256],
    ) -> Result<UpgradeKeyProofV1, RpcLifecycleError> {
        self.accept_finalized(at).await?;
        if at.height == 0 {
            return Err(invalid(ProofFailure::MissingAdmission));
        }
        let (target, opening) = self.state_proof(at, slots).await?;
        let history = self.admission_history(target).await?;
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
        self.accept_finalized(target).await?;
        Ok(proof)
    }
    pub(super) async fn onboarding(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
    ) -> Result<LifecycleOnboardingProofV1, RpcLifecycleError> {
        self.accept_finalized(at).await?;
        for hash in self.sealed_transactions() {
            if let Some((artifact, context)) = self.artifact_from_receipt(at, intent, hash).await? {
                return self.finish_onboarding(at, artifact, &context).await;
            }
        }
        let (artifact, context) = self.scan_sealed_offer(at, intent).await?;
        self.finish_onboarding(at, artifact, &context).await
    }
    async fn finish_onboarding(
        &self,
        at: LifecycleCheckpointV1,
        artifact: Bytes,
        context: &DcapOnboardingContextV1,
    ) -> Result<LifecycleOnboardingProofV1, RpcLifecycleError> {
        let proof = self
            .assemble(at, &onboarding_registry_slots_v1(context))
            .await?;
        Ok(LifecycleOnboardingProofV1 { artifact, proof })
    }
    async fn artifact_from_receipt(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
        tx_hash: B256,
    ) -> Result<Option<(Bytes, DcapOnboardingContextV1)>, RpcLifecycleError> {
        let Some(receipt) = self.raw_receipt(tx_hash).await? else {
            return Ok(None);
        };
        self.artifact_in_receipt(at, intent, tx_hash, receipt).await
    }
    async fn artifact_in_receipt(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
        tx_hash: B256,
        receipt: Value,
    ) -> Result<Option<(Bytes, DcapOnboardingContextV1)>, RpcLifecycleError> {
        let height = number(&receipt, "blockNumber")?;
        if height > at.height || hash(&receipt, "transactionHash")? != tx_hash {
            return Ok(None);
        }
        let block_hash = hash(&receipt, "blockHash")?;
        self.canonical_block(height, block_hash).await?;
        if number(&receipt, "status")? != 1 {
            return Ok(None);
        }
        self.unique_sealed_log(at, intent, tx_hash, &receipt).await
    }
    async fn unique_sealed_log(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
        tx_hash: B256,
        receipt: &Value,
    ) -> Result<Option<(Bytes, DcapOnboardingContextV1)>, RpcLifecycleError> {
        let Some(logs) = receipt.get("logs").and_then(Value::as_array) else {
            return Ok(None);
        };
        let height = number(receipt, "blockNumber")?;
        let block_hash = hash(receipt, "blockHash")?;
        let mut found = None;
        for log in logs {
            reject_receipt(
                hash(log, "transactionHash")? != tx_hash
                    || number(log, "blockNumber")? != height
                    || hash(log, "blockHash")? != block_hash,
            )?;
            if let Some(hit) = self.accept_sealed_log(at, intent, log).await? {
                reject_ambiguous(found.is_some())?;
                found = Some(hit);
            }
        }
        Ok(found)
    }
    async fn raw_receipt(&self, tx_hash: B256) -> Result<Option<Value>, RpcLifecycleError> {
        Ok(self
            .client
            .request("eth_getTransactionReceipt", rpc_params![tx_hash])
            .await?)
    }
    async fn scan_sealed_offer(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
    ) -> Result<(Bytes, DcapOnboardingContextV1), RpcLifecycleError> {
        let mut found = None;
        let mut end = at.height;
        for _ in 0..MAX_REVERSE_CHUNKS {
            let start = end.saturating_sub(LOG_CHUNK - 1);
            let logs: Vec<Value> = self
                .client
                .request(
                    "eth_getLogs",
                    rpc_params![json!({
                        "address": TEE_REGISTRY_ADDRESS,
                        "topics": [ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE_HASH],
                        "fromBlock": format!("0x{start:x}"),
                        "toBlock": format!("0x{end:x}"),
                    })],
                )
                .await?;
            for log in &logs {
                if let Some(hit) = self.accept_sealed_log(at, intent, log).await? {
                    if found.is_some() {
                        return Err(invalid(ProofFailure::AmbiguousArtifact));
                    }
                    found = Some(hit);
                }
            }
            if found.is_some() || start == 0 {
                break;
            }
            end = start.saturating_sub(1);
        }
        found.ok_or_else(|| invalid(ProofFailure::MissingArtifact))
    }
    async fn accept_sealed_log(
        &self,
        at: LifecycleCheckpointV1,
        intent: B256,
        log: &Value,
    ) -> Result<Option<(Bytes, DcapOnboardingContextV1)>, RpcLifecycleError> {
        let topics: Vec<B256> = serde_json::from_value(
            log.get("topics")
                .cloned()
                .ok_or(RpcLifecycleError::Missing("topics"))?,
        )?;
        let address: Address = serde_json::from_value(
            log.get("address")
                .cloned()
                .ok_or(RpcLifecycleError::Missing("address"))?,
        )?;
        // registerEnclave emits several events. Foreign logs are not ABI errors.
        if address != TEE_REGISTRY_ADDRESS
            || topics.first() != Some(&ITeeRegistryV1::OfferKeySealedForRegistryV1::SIGNATURE_HASH)
        {
            return Ok(None);
        }
        let event = ITeeRegistryV1::OfferKeySealedForRegistryV1::decode_raw_log_validate(
            topics,
            &bytes(log, "data")?,
        )?;
        let artifact = DcapOnboardingArtifactV1::decode_canonical(&event.sealedOfferKey)
            .map_err(RpcLifecycleError::Dcap)?;
        if artifact.context.intent_hash != intent {
            return Ok(None);
        }
        self.context_identity(&artifact.context)?;
        if event.nodeIdHash != artifact.context.node_id_hash
            || log.get("removed").and_then(Value::as_bool) != Some(false)
        {
            return Err(invalid(ProofFailure::ArtifactMismatch));
        }
        let receipt = self
            .receipt(at, hash(log, "transactionHash")?)
            .await?
            .ok_or_else(|| invalid(ProofFailure::InvalidReceipt))?;
        if !receipt.success
            || receipt.block_height != number(log, "blockNumber")?
            || receipt.block_hash != hash(log, "blockHash")?
        {
            return Err(invalid(ProofFailure::InvalidReceipt));
        }
        Ok(Some((event.sealedOfferKey, artifact.context)))
    }
}

fn reject_receipt(failed: bool) -> Result<(), RpcLifecycleError> {
    if failed {
        Err(invalid(ProofFailure::InvalidReceipt))
    } else {
        Ok(())
    }
}

fn reject_ambiguous(failed: bool) -> Result<(), RpcLifecycleError> {
    if failed {
        Err(invalid(ProofFailure::AmbiguousArtifact))
    } else {
        Ok(())
    }
}

fn proof_window_expired(error: &jsonrpsee::core::client::Error) -> bool {
    match error {
        jsonrpsee::core::client::Error::Call(object) => {
            object.message().contains("exceeds maximum proof window")
        }
        _ => false,
    }
}
