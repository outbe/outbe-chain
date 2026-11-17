use super::proof::invalid;
use super::*;
use alloy_consensus::{private::alloy_rlp, BlockHeader};
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_tee::finalized_admission::*;

impl RpcLifecyclePort {
    async fn certified(
        &self,
        height: u64,
    ) -> Result<(outbe_consensus::block::ConsensusBlock, CertifiedHeaderV1), RpcLifecycleError>
    {
        let public: Value = match self
            .client
            .request("outbe_getFinalityProof", rpc_params![height])
            .await
        {
            Err(jsonrpsee::core::client::Error::Call(e)) if e.code() == -32601 => {
                self.client
                    .request("outbe_getFinalization", rpc_params![height])
                    .await?
            }
            result => result?,
        };
        let finalization = bytes(&public, "finalizationHex")?;
        let encoded = bytes(&public, "blockHex")?;
        let certified = outbe_consensus::committee_chain::decode_public_finalized_block(
            &finalization,
            &encoded,
            outbe_consensus::bls::MAX_COMMITTEE_MEMBERS,
        )
        .map_err(|e| RpcLifecycleError::Finality(e.into()))?;
        if certified.finalization.proposal.payload != certified.block.digest() {
            return Err(invalid(ProofFailure::InvalidEnvelope));
        }
        let mut blocks = Vec::new();
        if let Some(ancestors) = public.get("ancestorBlocksHex") {
            let array = ancestors
                .as_array()
                .ok_or(RpcLifecycleError::Missing("ancestorBlocksHex array"))?;
            if array.len() > MAX_FINALITY_DESCENDANTS {
                return Err(invalid(ProofFailure::InvalidEnvelope));
            }
            for ancestor in array {
                let raw: Bytes = serde_json::from_value(ancestor.clone())?;
                blocks.push(
                    outbe_consensus::committee_chain::decode_public_block(&raw)
                        .map_err(|e| RpcLifecycleError::Finality(e.into()))?,
                );
            }
        }
        blocks.push(certified.block);
        if blocks[0].number() != height
            || blocks.windows(2).any(|p| {
                p[0].number().checked_add(1) != Some(p[1].number())
                    || p[1].parent_digest() != p[0].digest()
            })
        {
            return Err(invalid(ProofFailure::InvalidEnvelope));
        }
        let target = blocks[0].clone();
        let canonical = self.block(json!(format!("0x{height:x}"))).await?;
        if hash(&canonical, "hash")? != target.block_hash() {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let compact = CertifiedHeaderV1 {
            finalization,
            header: alloy_rlp::encode(target.header()),
            descendants: blocks
                .iter()
                .skip(1)
                .map(|b| alloy_rlp::encode(b.header()))
                .collect(),
        };
        Ok((target, compact))
    }
}

#[derive(Default)]
pub(super) struct AdmissionHistory {
    anchor: Option<Bytes>,
    transitions: Vec<Bytes>,
    admission: Option<CertifiedHeaderV1>,
}
impl AdmissionHistory {
    fn needs_certificate(
        &self,
        height: u64,
        at: LifecycleCheckpointV1,
        artifact: &Option<ConsensusHeaderArtifact>,
    ) -> bool {
        if height == at.height {
            return true;
        }
        match artifact {
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) => {
                *epoch == self.next_epoch()
            }
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                boundary.epoch == 0 && self.anchor.is_none()
            }
            _ => false,
        }
    }
    fn next_epoch(&self) -> u64 {
        self.transitions.len() as u64 + 1
    }
    fn record_artifact(
        &mut self,
        at: LifecycleCheckpointV1,
        block: &outbe_consensus::block::ConsensusBlock,
        compact: &CertifiedHeaderV1,
    ) -> Result<(), RpcLifecycleError> {
        let artifacts = decode_outbe_block_artifacts(block.header().extra_data())?;
        match artifacts.consensus_header_artifact {
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))
                if boundary.epoch == 0 && self.anchor.is_none() =>
            {
                self.anchor = Some(boundary.outcome)
            }
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. })
                if block.number() < at.height && epoch == self.next_epoch() =>
            {
                self.transitions.push(compact.encode_canonical()?.into());
                if self.transitions.len() > outbe_tee::upgrade_transfer::MAX_UPGRADE_COMMITTEES {
                    return Err(invalid(ProofFailure::InvalidEnvelope));
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn record_admission(
        &mut self,
        at: LifecycleCheckpointV1,
        block: &outbe_consensus::block::ConsensusBlock,
        compact: CertifiedHeaderV1,
    ) -> Result<(), RpcLifecycleError> {
        if block.number() == at.height {
            if block.block_hash() != at.hash || block.header().state_root() != at.state_root {
                return Err(RpcLifecycleError::CheckpointMismatch);
            }
            self.admission = Some(compact);
        }
        Ok(())
    }
    pub(super) fn finish(
        self,
    ) -> Result<(Bytes, Vec<Bytes>, CertifiedHeaderV1), RpcLifecycleError> {
        let admission = self
            .admission
            .ok_or_else(|| invalid(ProofFailure::MissingAdmission))?;
        let anchor = self
            .anchor
            .ok_or_else(|| invalid(ProofFailure::MissingAnchor))?;
        Ok((anchor, self.transitions, admission))
    }
}
impl RpcLifecyclePort {
    pub(super) async fn admission_history(
        &self,
        at: LifecycleCheckpointV1,
    ) -> Result<AdmissionHistory, RpcLifecycleError> {
        let mut history = AdmissionHistory::default();
        for height in 1..=at.height {
            let header = self.block(json!(format!("0x{height:x}"))).await?;
            if number(&header, "number")? != height {
                return Err(RpcLifecycleError::CheckpointMismatch);
            }
            let discovery = decode_outbe_block_artifacts(&bytes(&header, "extraData")?)?;
            if history.needs_certificate(height, at, &discovery.consensus_header_artifact) {
                let (block, compact) = self.certified(height).await?;
                history.record_artifact(at, &block, &compact)?;
                history.record_admission(at, &block, compact)?;
            }
        }
        Ok(history)
    }
}
