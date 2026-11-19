use super::proof::invalid;
use super::*;
use alloy_consensus::BlockHeader;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_tee::finalized_admission::*;

#[path = "history_replay.rs"]
mod replay;

#[derive(Clone, Default)]
pub(super) struct AdmissionHistory {
    anchor: Option<Bytes>,
    transitions: Vec<Bytes>,
    admission: Option<CertifiedHeaderV1>,
    /// Verified CommitteePreAnnounce at the admitted height. The witness excludes
    /// it until a later certificate's epoch is that announced committee. The
    /// block after the preannounce can still be signed by the outgoing committee.
    boundary_transition: Option<Bytes>,
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
        (self.transitions.len() + usize::from(self.boundary_transition.is_some())) as u64 + 1
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
                if block.number() <= at.height && epoch == self.next_epoch() =>
            {
                let encoded: Bytes = compact.encode_canonical()?.into();
                self.boundary_transition = Some(encoded);
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
    fn promote_boundary(&mut self) -> Result<(), RpcLifecycleError> {
        let Some(encoded) = self.boundary_transition.take() else {
            return Ok(());
        };
        self.transitions.push(encoded);
        if self.transitions.len() > outbe_tee::upgrade_transfer::MAX_UPGRADE_COMMITTEES {
            return Err(invalid(ProofFailure::InvalidEnvelope));
        }
        Ok(())
    }
}
pub(super) struct VerifierCheckpoint {
    chain: outbe_consensus::committee_chain::CommitteeChain,
    applied: usize,
}

pub(super) struct CachedHistory {
    height: u64,
    hash: B256,
    admitted_height: u64,
    history: AdmissionHistory,
    verifier: Option<VerifierCheckpoint>,
}

enum HistoryScan {
    Done(AdmissionHistory),
    Restart,
}

impl RpcLifecyclePort {
    fn cached_view(&self) -> Option<(u64, B256, u64, AdmissionHistory)> {
        let guard = self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cached = guard.as_ref()?;
        Some((
            cached.height,
            cached.hash,
            cached.admitted_height,
            cached.history.clone(),
        ))
    }
    fn take_verifier(&self) -> Option<VerifierCheckpoint> {
        self.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
            .and_then(|cached| cached.verifier.take())
    }
    fn set_verifier(&self, verifier: Option<VerifierCheckpoint>) {
        let mut guard = self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard.as_mut() {
            cached.verifier = verifier;
            return;
        }
        if let Some(verifier) = verifier {
            *guard = Some(CachedHistory {
                height: u64::MAX,
                hash: B256::ZERO,
                admitted_height: 0,
                history: AdmissionHistory::default(),
                verifier: Some(verifier),
            });
        }
    }
    fn store_history(
        &self,
        height: u64,
        hash: B256,
        admitted_height: u64,
        history: &AdmissionHistory,
    ) {
        let mut guard = self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let verifier = guard.as_mut().and_then(|cached| cached.verifier.take());
        *guard = Some(CachedHistory {
            height,
            hash,
            admitted_height,
            history: history.clone(),
            verifier,
        });
    }
    fn clear_history(&self) {
        *self
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
    async fn resume(
        &self,
        at: LifecycleCheckpointV1,
    ) -> Result<(AdmissionHistory, u64, Option<B256>), RpcLifecycleError> {
        let Some((height, cached_hash, admitted_height, history)) = self.cached_view() else {
            return Ok((AdmissionHistory::default(), 1, None));
        };
        if height > at.height {
            self.clear_history();
            return Ok((AdmissionHistory::default(), 1, None));
        }
        if admitted_height == height && height < at.height {
            return Ok((history, height.saturating_add(1), Some(cached_hash)));
        }
        let header = self.block(json!(format!("0x{height:x}"))).await?;
        if number(&header, "number")? != height || hash(&header, "hash")? != cached_hash {
            self.clear_history();
            return Ok((AdmissionHistory::default(), 1, None));
        }
        if height == at.height && admitted_height == at.height {
            return Ok((history, height.saturating_add(1), None));
        }
        let start = if admitted_height == 0 && height == at.height {
            height
        } else {
            height.saturating_add(1)
        };
        Ok((history, start, Some(cached_hash)))
    }
    async fn linked_header(
        &self,
        height: u64,
        parent: Option<B256>,
    ) -> Result<Option<(serde_json::Value, B256)>, RpcLifecycleError> {
        let header = self.block(json!(format!("0x{height:x}"))).await?;
        if number(&header, "number")? != height {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let block_hash = hash(&header, "hash")?;
        let parent_hash = hash(&header, "parentHash")?;
        if parent.is_some_and(|expected| expected != parent_hash) {
            self.clear_history();
            return Ok(None);
        }
        Ok(Some((header, block_hash)))
    }
    async fn ingest_certificate(
        &self,
        at: LifecycleCheckpointV1,
        history: &mut AdmissionHistory,
        height: u64,
        artifact: &Option<ConsensusHeaderArtifact>,
    ) -> Result<(), RpcLifecycleError> {
        if !history.needs_certificate(height, at, artifact) {
            return Ok(());
        }
        let (block, compact) = match self.certified(height, history).await {
            Ok(certified) => certified,
            Err(error @ RpcLifecycleError::Unauthenticated) => {
                self.clear_history();
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = history
            .record_artifact(at, &block, &compact)
            .and_then(|()| history.record_admission(at, &block, compact))
        {
            self.clear_history();
            return Err(error);
        }
        Ok(())
    }
    async fn scan_history(
        &self,
        at: LifecycleCheckpointV1,
    ) -> Result<HistoryScan, RpcLifecycleError> {
        let (mut history, mut height, mut parent) = self.resume(at).await?;
        while height <= at.height {
            let Some((header, block_hash)) = self.linked_header(height, parent).await? else {
                return Ok(HistoryScan::Restart);
            };
            let discovery = decode_outbe_block_artifacts(&bytes(&header, "extraData")?)?;
            self.ingest_certificate(
                at,
                &mut history,
                height,
                &discovery.consensus_header_artifact,
            )
            .await?;
            let admitted = if height == at.height { at.height } else { 0 };
            self.store_history(height, block_hash, admitted, &history);
            parent = Some(block_hash);
            height = height.saturating_add(1);
        }
        Ok(HistoryScan::Done(history))
    }
    pub(super) async fn admission_history(
        &self,
        at: LifecycleCheckpointV1,
    ) -> Result<AdmissionHistory, RpcLifecycleError> {
        if let HistoryScan::Done(history) = self.scan_history(at).await? {
            return Ok(history);
        }
        self.clear_history();
        match self.scan_history(at).await? {
            HistoryScan::Done(history) => Ok(history),
            HistoryScan::Restart => Err(RpcLifecycleError::CheckpointMismatch),
        }
    }
}
