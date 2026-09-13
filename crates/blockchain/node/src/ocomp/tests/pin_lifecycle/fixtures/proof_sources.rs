use super::*;

#[derive(Clone, Default)]
pub(in super::super) struct DeterministicProofSource {
    jobs: Arc<Mutex<BTreeMap<B256, (CandidatePinV1, B256)>>>,
    finalized: Arc<Mutex<BTreeMap<u64, (B256, B256)>>>,
    pub(in super::super) terminal: Arc<Mutex<BTreeMap<B256, u64>>>,
    finality_available: Arc<AtomicBool>,
}

impl DeterministicProofSource {
    pub(in super::super) fn with_jobs(
        jobs: impl IntoIterator<Item = (CandidatePinV1, B256)>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(
                jobs.into_iter()
                    .map(|(candidate, job_id)| (candidate.block_hash, (candidate, job_id)))
                    .collect(),
            )),
            finalized: Arc::new(Mutex::new(BTreeMap::new())),
            terminal: Arc::new(Mutex::new(BTreeMap::new())),
            finality_available: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(in super::super) fn set_finality_available(&self, available: bool) {
        self.finality_available.store(available, Ordering::SeqCst);
    }

    pub(in super::super) fn observe_finalized(&self, block: &ConsensusBlock) {
        self.finalized
            .lock()
            .expect("deterministic finality lock")
            .insert(block.number(), (block.block_hash(), block.parent_hash()));
    }

    pub(in super::super) fn observe_terminal(&self, job_id: B256, terminal_height: u64) {
        self.terminal
            .lock()
            .expect("deterministic terminal lock")
            .insert(job_id, terminal_height);
    }
}

impl FinalizedInputProofSource for DeterministicProofSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok(self
            .jobs
            .lock()
            .expect("deterministic source lock")
            .get(&block.block_hash())
            .map(|(candidate, _)| *candidate))
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if !self.finality_available.load(Ordering::SeqCst) {
            return Err(RetentionError::Source(
                "finalization proof is unavailable".to_owned(),
            ));
        }
        let finalized = self.finalized.lock().expect("deterministic finality lock");
        let (finality_recorded_height, finalized_hash) =
            if let Some((hash, _)) = finalized.get(&candidate.block_number) {
                (candidate.block_number, *hash)
            } else {
                let successor_height = candidate
                    .block_number
                    .checked_add(1)
                    .ok_or_else(|| RetentionError::Source("finality height overflow".to_owned()))?;
                let (_, parent_hash) = finalized.get(&successor_height).ok_or_else(|| {
                    RetentionError::Source("candidate-height finality is unavailable".to_owned())
                })?;
                (successor_height, *parent_hash)
            };
        if finalized_hash != candidate.block_hash {
            return Ok(CandidateFinalityV1::Orphaned);
        }
        let jobs = self.jobs.lock().expect("deterministic source lock");
        let (expected, job_id) = jobs
            .get(&candidate.block_hash)
            .copied()
            .ok_or_else(|| RetentionError::Source("unknown finalized fixture".to_owned()))?;
        if expected != candidate {
            return Err(RetentionError::Source(
                "finalized fixture differs from candidate".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id,
            finality_recorded_height,
            open_height: finality_recorded_height + 4,
            deadline_height: finality_recorded_height + 14,
        }))
    }

    fn terminal_height_at(
        &self,
        _block: &ConsensusBlock,
        _candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Ok(self
            .terminal
            .lock()
            .expect("deterministic terminal lock")
            .get(&job_id)
            .copied())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum VoteOutcome {
    Positive,
    Abstained,
}

pub(in super::super) struct DeterministicConsensusDriver;

impl DeterministicConsensusDriver {
    pub(in super::super) fn vote(
        coordinator: &OcompRetentionCoordinator,
        block: &ConsensusBlock,
    ) -> VoteOutcome {
        coordinator
            .prepare_candidate(block)
            .map(|()| VoteOutcome::Positive)
            .unwrap_or(VoteOutcome::Abstained)
    }

    pub(in super::super) fn finalize(
        source: &DeterministicProofSource,
        coordinator: &OcompRetentionCoordinator,
        block: &ConsensusBlock,
    ) {
        source.observe_finalized(block);
        coordinator
            .reconcile_finalized(block)
            .expect("finalization notification is node-local and must reconcile");
    }
}
