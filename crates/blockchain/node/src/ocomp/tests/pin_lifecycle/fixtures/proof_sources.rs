use super::*;

#[derive(Clone, Default)]
pub(in super::super) struct DeterministicProofSource {
    jobs: Arc<Mutex<BTreeMap<B256, (CandidatePinV1, B256)>>>,
    intents: Arc<BTreeMap<B256, JobIntentV1>>,
    pub(in super::super) terminal: Arc<Mutex<BTreeMap<B256, u64>>>,
}

impl DeterministicProofSource {
    pub(in super::super) fn with_jobs(
        jobs: impl IntoIterator<Item = (CandidatePinV1, B256)>,
    ) -> Self {
        Self::with_intents(jobs.into_iter().map(|(candidate, job_id)| {
            assert_eq!(fixture_job_id(candidate), job_id);
            (candidate, intent_for_candidate(candidate))
        }))
    }

    pub(in super::super) fn with_intents(
        records: impl IntoIterator<Item = (CandidatePinV1, JobIntentV1)>,
    ) -> Self {
        let mut jobs = BTreeMap::new();
        let mut intents = BTreeMap::new();
        for (candidate, intent) in records {
            assert_eq!(
                intent.intent_id(&poc_schema_limits()).unwrap(),
                candidate.intent_id
            );
            assert_eq!(intent.input_lease_id().unwrap(), candidate.input_lease_id);
            jobs.insert(candidate.block_hash, (candidate, fixture_job_id(candidate)));
            intents.insert(candidate.block_hash, intent);
        }
        Self {
            jobs: Arc::new(Mutex::new(jobs)),
            intents: Arc::new(intents),
            terminal: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(in super::super) fn observe_terminal(&self, job_id: B256, height: u64) {
        self.terminal.lock().unwrap().insert(job_id, height);
    }

    pub(in super::super) fn frame(&self, block: &ConsensusBlock) -> FinalizedFrame {
        let jobs = self.jobs.lock().unwrap();
        let receipts = jobs
            .get(&block.block_hash())
            .map(|(candidate, _)| {
                let intent = &self.intents[&candidate.block_hash];
                let event = IMetadosis::OffchainJobRequested {
                    intentId: candidate.intent_id,
                    wwd: intent.wwd,
                    pendingNonce: intent.pending_nonce,
                    attempt: intent.attempt,
                    activationPreconditionsHash: intent
                        .activation_preconditions
                        .activation_preconditions_hash(&poc_schema_limits())
                        .unwrap(),
                };
                Receipt {
                    tx_type: TxType::Legacy,
                    success: true,
                    cumulative_gas_used: 1,
                    logs: vec![Log {
                        address: METADOSIS_ADDRESS,
                        data: event.encode_log_data(),
                    }],
                }
            })
            .into_iter()
            .collect();
        frame_for_block(block, receipts)
    }

    pub(in super::super) fn canonical_job(&self, candidate: CandidatePinV1) -> OcompJobRecordV1 {
        let job_id = self
            .jobs
            .lock()
            .unwrap()
            .get(&candidate.block_hash)
            .unwrap()
            .1;
        OcompJobRecordV1 {
            intent: self.intents[&candidate.block_hash].clone(),
            intent_height: candidate.block_number,
            status: OcompJobStatus::AwaitingFinality,
            finalized: Some(OcompFinalizedJobV1 {
                job_id,
                finalized_request_block_hash: candidate.block_hash,
                finalized_request_state_root: candidate.state_root,
                finality_recorded_height: candidate.block_number + 1,
                open_height: candidate.block_number + 5,
                deadline_height: candidate.block_number + 15,
                quorum: None,
            }),
            terminal: None,
        }
    }
}

impl FinalizedInputProofSource for DeterministicProofSource {
    fn candidate_for_finalized_observation(
        &self,
        frame: &FinalizedFrame,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        let candidate = self
            .jobs
            .lock()
            .unwrap()
            .get(&frame.identity().hash)
            .map(|(candidate, _)| *candidate)
            .ok_or_else(|| RetentionError::Source("unknown finalized fixture".to_owned()))?;
        let intent = &self.intents[&candidate.block_hash];
        if frame.identity().number != candidate.block_number
            || frame.state_root() != candidate.state_root
            || observation.intent_id != candidate.intent_id
            || observation.wwd != candidate.wwd
            || observation.pending_nonce != intent.pending_nonce
            || observation.attempt != intent.attempt
            || observation.activation_preconditions_hash
                != intent
                    .activation_preconditions
                    .activation_preconditions_hash(&poc_schema_limits())
                    .unwrap()
        {
            return Err(RetentionError::Source(
                "finalized event does not match fixture state".to_owned(),
            ));
        }
        Ok(candidate)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        _frame: &FinalizedFrame,
        _candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Ok(self.terminal.lock().unwrap().get(&job_id).copied())
    }
}

pub(in super::super) struct FinalizedFrameDriver;

impl FinalizedFrameDriver {
    pub(in super::super) fn admit(
        source: &DeterministicProofSource,
        coordinator: &OcompRetentionCoordinator,
        block: &ConsensusBlock,
    ) -> Result<(), RetentionError> {
        let frame = source.frame(block);
        let observation = observe_finalized_request(&frame)?;
        coordinator.reconcile_finalized_frame(&frame, observation)
    }

    pub(in super::super) fn bind(
        source: &DeterministicProofSource,
        coordinator: &OcompRetentionCoordinator,
        block: &ConsensusBlock,
    ) {
        let candidate = source
            .jobs
            .lock()
            .unwrap()
            .get(&block.block_hash())
            .unwrap()
            .0;
        coordinator
            .bind_canonical_finalized_job(block.block_hash(), &source.canonical_job(candidate))
            .unwrap();
    }
}
