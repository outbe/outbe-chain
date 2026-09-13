use crate::ocomp::retention::*;

type PendingCandidateReceipts = Option<(B256, Vec<OutbeReceipt>)>;

type PendingCandidateReceiptReader =
    dyn Fn() -> Result<PendingCandidateReceipts, String> + Send + Sync;

/// Typed exact-block source used by the retention coordinator.
///
/// OCM-10 extends this seam with bounded raw proof/opening construction. OCM-09
/// uses only the two operations necessary to authenticate one tentative record
/// and derive its finalized JobId.
pub trait FinalizedInputProofSource: Send + Sync {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError>;

    /// Authenticate the one request observation decoded by the unified
    /// finalized-frame reader. Production finalized reconciliation must use
    /// this seam and must not traverse receipts again.
    fn candidate_for_finalized_observation(
        &self,
        _frame: &FinalizedFrame,
        _observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        Err(RetentionError::Source(
            "finalized-frame candidate authentication is unavailable".to_owned(),
        ))
    }

    /// Resolve a tentative candidate only from persisted consensus finality.
    ///
    /// An unavailable or ambiguous proof is an error and leaves the candidate
    /// tentative/non-signable. `Orphaned` requires an exact competing
    /// finalization at the candidate height; live canonical state is never
    /// enough.
    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError>;

    fn terminal_height_at(
        &self,
        _block: &ConsensusBlock,
        _candidate: CandidatePinV1,
        _job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Ok(None)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        _frame: &FinalizedFrame,
        _candidate: CandidatePinV1,
        _job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Err(RetentionError::Source(
            "finalized-frame terminal observation is unavailable".to_owned(),
        ))
    }

    fn build_finalized_intent_proof(
        &self,
        _candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        Err(RetentionError::Source(
            "finalized-intent proof construction is unavailable".to_owned(),
        ))
    }

    fn build_lysis_openings(
        &self,
        _candidate: CandidatePinV1,
        _subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        Err(RetentionError::Source(
            "Lysis opening construction is unavailable".to_owned(),
        ))
    }
}

/// Reth-backed typed state source. Request events are locators, never authority.
pub struct RethFinalizedInputProofSource<P> {
    pub(in crate::ocomp) provider: P,
    parent_proofs: FinalizedParentCertStore,
    pub(in crate::ocomp) proof_builder: RethFinalizedIntentProofBuilder<P>,
    pending_receipts: Arc<PendingCandidateReceiptReader>,
    result_deadline_blocks: u64,
    pub(in crate::ocomp) limits: SchemaLimits,
}

impl<P: Clone> RethFinalizedInputProofSource<P> {
    pub fn new(
        provider: P,
        parent_proofs: FinalizedParentCertStore,
        pending_receipts: impl Fn() -> Result<Option<(B256, Vec<OutbeReceipt>)>, String>
            + Send
            + Sync
            + 'static,
        result_deadline_blocks: u64,
    ) -> Self {
        Self {
            provider: provider.clone(),
            parent_proofs: parent_proofs.clone(),
            proof_builder: RethFinalizedIntentProofBuilder::new(
                provider,
                parent_proofs,
                poc_schema_limits(),
            ),
            pending_receipts: Arc::new(pending_receipts),
            result_deadline_blocks,
            limits: poc_schema_limits(),
        }
    }
}

impl<P> RethFinalizedInputProofSource<P>
where
    P: ReceiptProvider + StateProviderFactory + Send + Sync,
{
    fn record_at(
        &self,
        block: &ConsensusBlock,
        intent_id: B256,
    ) -> Result<OcompJobRecordV1, RetentionError> {
        self.record_at_hash(block.block_hash(), intent_id)
    }

    fn record_at_hash(
        &self,
        block_hash: B256,
        intent_id: B256,
    ) -> Result<OcompJobRecordV1, RetentionError> {
        read_ocomp_job_record_at(&self.provider, block_hash, intent_id, &self.limits)
    }

    fn candidate_from_observation(
        &self,
        block_number: u64,
        block_hash: B256,
        state_root: B256,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        let record = self.record_at_hash(block_hash, observation.intent_id)?;
        if record.status != OcompJobStatus::AwaitingFinality {
            return Err(RetentionError::Source(
                "event locator does not open the exact pending intent".to_owned(),
            ));
        }
        validate_request_observation(&record.intent, observation, &self.limits)?;
        Ok(CandidatePinV1 {
            block_number,
            block_hash,
            state_root,
            intent_id: observation.intent_id,
            wwd: record.intent.wwd,
            ce_sealed_root: record.intent.ce_sealed_root,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            input_lease_id: record
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?,
        })
    }
}

/// Decode `OffchainJobRequested` exactly once from a finalized frame shared by
/// projection, retention and discovery. More than one request in a block is a
/// protocol contradiction and fails closed.
pub fn observe_finalized_request(
    frame: &FinalizedFrame,
) -> Result<Option<FinalizedRequestObservationV1>, RetentionError> {
    observe_request_in_receipts(frame.receipts())
}

fn observe_request_in_receipts(
    receipts: &[OutbeReceipt],
) -> Result<Option<FinalizedRequestObservationV1>, RetentionError> {
    let mut observation = None;
    for receipt in receipts {
        if !receipt.status() {
            continue;
        }
        for log in receipt.logs() {
            if log.address != METADOSIS_ADDRESS
                || log.data.topics().first()
                    != Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
            {
                continue;
            }
            let event = IMetadosis::OffchainJobRequested::decode_log(log).map_err(|error| {
                RetentionError::Source(format!("decode finalized OCOMP request: {error}"))
            })?;
            let decoded = FinalizedRequestObservationV1 {
                intent_id: event.data.intentId,
                wwd: event.data.wwd,
                pending_nonce: event.data.pendingNonce,
                attempt: event.data.attempt,
                activation_preconditions_hash: event.data.activationPreconditionsHash,
            };
            if observation.replace(decoded).is_some() {
                return Err(RetentionError::Source(
                    "finalized frame contains more than one OCOMP request".to_owned(),
                ));
            }
        }
    }
    Ok(observation)
}

/// Read one exact typed OCOMP job record from canonical state at `block_hash`.
///
/// Events are locators only. Embedded Supervisor and retention both use this
/// single state decoder so neither can accidentally trust event payloads as the
/// job authority.
pub fn read_ocomp_job_record_at<P>(
    provider: &P,
    block_hash: B256,
    intent_id: B256,
    limits: &SchemaLimits,
) -> Result<OcompJobRecordV1, RetentionError>
where
    P: StateProviderFactory,
{
    let state = provider
        .state_by_block_hash(block_hash)
        .map_err(|error| RetentionError::Source(format!("open exact block state: {error}")))?;
    let logical_key = intent_storage_key(intent_id)
        .map_err(|error| RetentionError::Source(format!("derive intent slot: {error}")))?;
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    let encoded = read_storage_bytes(state.as_ref(), base, limits.max_bounded_bytes)?;
    let record = OcompJobRecordV1::decode_canonical(&encoded, limits)
        .map_err(|error| RetentionError::Source(format!("decode typed job record: {error}")))?;
    let decoded_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| RetentionError::Source(format!("hash typed job record: {error}")))?;
    if decoded_id != intent_id {
        return Err(RetentionError::Source(
            "storage key does not open the exact typed JobIntent".to_owned(),
        ));
    }
    Ok(record)
}

/// Resolve whether one local OCOMP key belongs to the job's exact historical
/// ValidatorSet snapshot at the request block. A promoted or re-entered
/// Validator must not submit a vote for a job whose pinned snapshot predates
/// that membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OcompSnapshotEligibilityV1 {
    Eligible,
    NotMember,
    Unavailable { detail: String },
    Corrupt { detail: String },
}

pub fn ocomp_snapshot_contains_key_at<P>(
    provider: &P,
    block_hash: B256,
    intent: &JobIntentV1,
    ocomp_key_hash: B256,
) -> OcompSnapshotEligibilityV1
where
    P: StateProviderFactory,
{
    if ocomp_key_hash.is_zero() {
        return OcompSnapshotEligibilityV1::Corrupt {
            detail: "local OCOMP key hash is zero".to_owned(),
        };
    }
    let state = match provider.state_by_block_hash(block_hash) {
        Ok(state) => state,
        Err(error) => {
            return OcompSnapshotEligibilityV1::Unavailable {
                detail: format!("open exact snapshot state: {error}"),
            };
        }
    };
    let reader = OcompSnapshotStateReader {
        state: state.as_ref(),
    };
    let mut readonly = ReadOnlyStorageProvider::new(reader);
    let storage = StorageHandle::new(&mut readonly);
    let extension = match outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        storage.clone(),
        intent.result_validator_set_epoch,
        intent.result_committee_set_hash,
        intent.result_ocomp_binding_hash,
    ) {
        Ok(Some(extension)) => extension,
        Ok(None) => {
            return OcompSnapshotEligibilityV1::Corrupt {
                detail: "pinned OCOMP snapshot is missing".to_owned(),
            };
        }
        Err(error) => return classify_snapshot_read_error("read pinned OCOMP snapshot", error),
    };
    if extension.member_count != intent.result_member_count {
        return OcompSnapshotEligibilityV1::Corrupt {
            detail: "pinned OCOMP snapshot member count disagrees with JobIntent".to_owned(),
        };
    }
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        intent.result_validator_set_epoch,
        intent.result_committee_set_hash,
    );
    for index in 0..extension.member_count {
        let member = match outbe_validatorset::read_ocomp_snapshot_member_at(
            storage.clone(),
            snapshot_key,
            index,
        ) {
            Ok(Some(member)) => member,
            Ok(None) => {
                return OcompSnapshotEligibilityV1::Corrupt {
                    detail: "pinned OCOMP member is missing".to_owned(),
                };
            }
            Err(error) => return classify_snapshot_read_error("read pinned OCOMP member", error),
        };
        if keccak256(member.ocomp_public_key_sec1) == ocomp_key_hash {
            return OcompSnapshotEligibilityV1::Eligible;
        }
    }
    OcompSnapshotEligibilityV1::NotMember
}

fn classify_snapshot_read_error(
    context: &'static str,
    error: PrecompileError,
) -> OcompSnapshotEligibilityV1 {
    match error {
        PrecompileError::Storage(detail) => OcompSnapshotEligibilityV1::Unavailable {
            detail: format!("{context}: {detail}"),
        },
        error => OcompSnapshotEligibilityV1::Corrupt {
            detail: format!("{context}: {error}"),
        },
    }
}

struct OcompSnapshotStateReader<'a> {
    state: &'a dyn StateProvider,
}

impl StorageReader for OcompSnapshotStateReader<'_> {
    fn read_storage(
        &self,
        address: alloy_primitives::Address,
        key: B256,
    ) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "pinned OCOMP snapshot state read failed: {error}"
                ))
            })
    }
}

impl<P> FinalizedInputProofSource for RethFinalizedInputProofSource<P>
where
    P: ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + HeaderProvider<Header = OutbeHeader>
        + Send
        + Sync,
{
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        let pending = (self.pending_receipts)().map_err(|error| {
            RetentionError::Source(format!("load pending candidate receipts: {error}"))
        })?;
        let receipts = match pending {
            Some((pending_hash, receipts)) if pending_hash == block.block_hash() => receipts,
            _ => self
                .provider
                .receipts_by_block(block.block_hash().into())
                .map_err(|error| {
                    RetentionError::Source(format!("load canonical candidate receipts: {error}"))
                })?
                .ok_or_else(|| {
                    RetentionError::Source(
                        "candidate receipts are unavailable from pending and canonical execution"
                            .to_owned(),
                    )
                })?,
        };
        let Some(observation) = observe_request_in_receipts(&receipts)? else {
            return Ok(None);
        };
        self.candidate_from_observation(
            block.number(),
            block.block_hash(),
            block.header().state_root(),
            observation,
        )
        .map(Some)
    }

    fn candidate_for_finalized_observation(
        &self,
        frame: &FinalizedFrame,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        let identity = frame.identity();
        self.candidate_from_observation(
            identity.number,
            identity.hash,
            frame.state_root(),
            observation,
        )
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        let records = self
            .parent_proofs
            .finalizations_at_height(candidate.block_number);
        if records.is_empty() {
            return Err(RetentionError::Source(
                "candidate-height finalization proof is unavailable".to_owned(),
            ));
        }
        let hashes = records
            .iter()
            .map(|record| record.finalized_block_hash)
            .collect::<BTreeSet<_>>();
        if hashes.len() != 1 {
            return Err(RetentionError::Source(
                "candidate-height finalization proofs disagree".to_owned(),
            ));
        }
        let finalized_hash = *hashes
            .first()
            .expect("non-empty finalization set has one hash");
        if finalized_hash != candidate.block_hash {
            return Ok(CandidateFinalityV1::Orphaned);
        }
        if records.len() != 1 {
            return Err(RetentionError::Source(
                "candidate has ambiguous finalization proof records".to_owned(),
            ));
        }
        let header = self
            .provider
            .sealed_header_by_hash(candidate.block_hash)
            .map_err(|error| {
                RetentionError::Source(format!("load finalized candidate header: {error}"))
            })?
            .ok_or_else(|| {
                RetentionError::Source("finalized candidate header is unavailable".to_owned())
            })?;
        if header.number() != candidate.block_number
            || header.hash() != candidate.block_hash
            || header.state_root() != candidate.state_root
        {
            return Err(RetentionError::Source(
                "finalized header does not match tentative source identity".to_owned(),
            ));
        }
        let (_, verified) = self
            .proof_builder
            .build_and_verify_header(header.header(), header.hash(), candidate.intent_id)
            .map_err(|error| {
                RetentionError::Source(format!(
                    "build and verify exact finalized intent proof: {error}"
                ))
            })?;
        if verified.request.block_number != candidate.block_number
            || verified.request.block_hash != candidate.block_hash
            || verified.request.state_root != candidate.state_root
            || verified.intent_id != candidate.intent_id
        {
            return Err(RetentionError::Source(
                "verified finalized intent differs from tentative source identity".to_owned(),
            ));
        }
        if verified.intent.wwd != candidate.wwd
            || verified.intent.ce_sealed_root != candidate.ce_sealed_root
            || verified.intent.protocol_bundle_hash != candidate.protocol_bundle_hash
            || verified
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
            || job_id_from_intent_id(
                candidate.intent_id,
                candidate.block_hash,
                candidate.state_root,
            )
            .map_err(|error| RetentionError::Source(format!("derive tentative JobId: {error}")))?
                != verified.job_id
        {
            return Err(RetentionError::Source(
                "finalized intent differs from tentative pin".to_owned(),
            ));
        }
        let finality_recorded_height = records[0].stored_at_height;
        let open_height = finality_recorded_height
            .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
            .ok_or_else(|| RetentionError::Source("voting-open height overflow".to_owned()))?;
        let deadline_height = open_height
            .checked_add(self.result_deadline_blocks)
            .ok_or_else(|| RetentionError::Source("result deadline height overflow".to_owned()))?;
        if self.result_deadline_blocks == 0 {
            return Err(RetentionError::Source(
                "result deadline window is zero".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id: verified.job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        }))
    }

    fn terminal_height_at(
        &self,
        block: &ConsensusBlock,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let record = self.record_at(block, candidate.intent_id)?;
        terminal_height_from_record(&record, block.number(), candidate, job_id)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let identity = frame.identity();
        let record = self.record_at_hash(identity.hash, candidate.intent_id)?;
        terminal_height_from_record(&record, identity.number, candidate, job_id)
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        let (proof, verified) = self
            .proof_builder
            .build_and_verify_header(
                self.provider
                    .sealed_header_by_hash(candidate.block_hash)
                    .map_err(|error| {
                        RetentionError::Source(format!("load finalized opening header: {error}"))
                    })?
                    .ok_or_else(|| {
                        RetentionError::Source("finalized opening header is unavailable".to_owned())
                    })?
                    .header(),
                candidate.block_hash,
                candidate.intent_id,
            )
            .map_err(|error| RetentionError::Source(error.to_string()))?;
        if verified.job_id != candidate_job_id(candidate)?
            || verified
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
        {
            return Err(RetentionError::Source(
                "finalized-intent proof opens a different JobId".to_owned(),
            ));
        }
        Ok(proof)
    }

    fn build_lysis_openings(
        &self,
        candidate: CandidatePinV1,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        crate::ocomp::openings::build_lysis_openings(
            &self.provider,
            &self.limits,
            candidate,
            subjects,
        )
    }
}

pub(in crate::ocomp::retention) fn canonical_finalized_pin(
    candidate: CandidatePinV1,
    record: &OcompJobRecordV1,
) -> Result<FinalizedJobPinV1, RetentionError> {
    let limits = poc_schema_limits();
    record.validate_semantics(&limits).map_err(|error| {
        RetentionError::Source(format!("validate canonical finalized OCOMP job: {error}"))
    })?;
    let finalized = record
        .finalized
        .as_ref()
        .ok_or(RetentionError::InvalidTransition(
            "canonical OCOMP job is not finalized",
        ))?;
    let intent_id = record
        .intent
        .intent_id(&limits)
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    let input_lease_id = record
        .intent
        .input_lease_id()
        .map_err(|error| RetentionError::Source(error.to_string()))?;
    if record.intent_height != candidate.block_number
        || intent_id != candidate.intent_id
        || record.intent.wwd != candidate.wwd
        || record.intent.ce_sealed_root != candidate.ce_sealed_root
        || record.intent.protocol_bundle_hash != candidate.protocol_bundle_hash
        || input_lease_id != candidate.input_lease_id
        || finalized.finalized_request_block_hash != candidate.block_hash
        || finalized.finalized_request_state_root != candidate.state_root
    {
        return Err(RetentionError::InvalidTransition(
            "canonical finalized job does not match retained request candidate",
        ));
    }
    Ok(FinalizedJobPinV1 {
        candidate,
        job_id: finalized.job_id,
        finality_recorded_height: finalized.finality_recorded_height,
        open_height: finalized.open_height,
        deadline_height: finalized.deadline_height,
    })
}

pub(in crate::ocomp::retention) fn terminal_height_from_record(
    record: &OcompJobRecordV1,
    observed_height: u64,
    candidate: CandidatePinV1,
    job_id: B256,
) -> Result<Option<u64>, RetentionError> {
    if record
        .intent
        .input_lease_id()
        .map_err(|error| RetentionError::Source(error.to_string()))?
        != candidate.input_lease_id
    {
        return Err(RetentionError::Source(
            "terminal JobIntent changed its authenticated input lease".to_owned(),
        ));
    }
    match record.status {
        OcompJobStatus::AwaitingFinality | OcompJobStatus::VotingOpen => Ok(None),
        OcompJobStatus::Completed | OcompJobStatus::Expired | OcompJobStatus::Failed => {
            let terminal = record.terminal.as_ref().ok_or_else(|| {
                RetentionError::Source("terminal Job is missing terminal record".to_owned())
            })?;
            if terminal.terminal_height > observed_height {
                return Err(RetentionError::Source(
                    "terminal Job binding differs from retained finalized Job".to_owned(),
                ));
            }
            let deadline_height = if let Some(finalized) = record.finalized.as_ref() {
                if finalized.job_id != job_id
                    || finalized.finalized_request_block_hash != candidate.block_hash
                    || finalized.finalized_request_state_root != candidate.state_root
                {
                    return Err(RetentionError::Source(
                        "terminal Job binding differs from retained finalized Job".to_owned(),
                    ));
                }
                finalized.deadline_height
            } else if record.status == OcompJobStatus::Completed {
                return Err(RetentionError::Source(
                    "completed Job is missing finalized binding".to_owned(),
                ));
            } else {
                terminal.terminal_height
            };
            retention_terminal_height_for_status(
                record.status,
                observed_height,
                deadline_height,
                terminal.terminal_height,
            )
        }
    }
}

fn validate_request_observation(
    intent: &JobIntentV1,
    observation: FinalizedRequestObservationV1,
    limits: &SchemaLimits,
) -> Result<(), RetentionError> {
    let activation_hash = intent
        .activation_preconditions
        .activation_preconditions_hash(limits)
        .map_err(|error| {
            RetentionError::Source(format!("hash activation preconditions: {error}"))
        })?;
    if intent.wwd != observation.wwd
        || intent.pending_nonce != observation.pending_nonce
        || intent.attempt != observation.attempt
        || activation_hash != observation.activation_preconditions_hash
    {
        return Err(RetentionError::Source(
            "request event locator disagrees with typed state".to_owned(),
        ));
    }
    Ok(())
}

fn read_storage_bytes(
    state: &dyn reth_storage_api::StateProvider,
    base: U256,
    max_len: usize,
) -> Result<Vec<u8>, RetentionError> {
    let base_key = B256::new(base.to_be_bytes::<32>());
    let word = state
        .storage(METADOSIS_ADDRESS, base_key)
        .map_err(|error| RetentionError::Source(format!("read job record base slot: {error}")))?
        .unwrap_or_default();
    let encoded_word = word.to_be_bytes::<32>();
    if encoded_word[31] & 1 == 0 {
        let len = usize::from(encoded_word[31] / 2);
        if len > 31 || len > max_len || encoded_word[len..31].iter().any(|byte| *byte != 0) {
            return Err(RetentionError::Source(
                "non-canonical inline StorageBytes".to_owned(),
            ));
        }
        return Ok(encoded_word[..len].to_vec());
    }

    let encoded_len = word
        .checked_sub(U256::from(1))
        .ok_or_else(|| RetentionError::Source("invalid StorageBytes length word".to_owned()))?
        / U256::from(2);
    if encoded_len > U256::from(max_len) {
        return Err(RetentionError::Source(
            "job record exceeds bounded StorageBytes length".to_owned(),
        ));
    }
    let len = encoded_len.to::<usize>();
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    let mut encoded = Vec::with_capacity(len);
    for index in 0..len.div_ceil(32) {
        let slot = data_base + U256::from(index);
        let chunk = state
            .storage(METADOSIS_ADDRESS, B256::new(slot.to_be_bytes::<32>()))
            .map_err(|error| {
                RetentionError::Source(format!("read job record data slot {index}: {error}"))
            })?
            .unwrap_or_default()
            .to_be_bytes::<32>();
        let remaining = len - encoded.len();
        let take = remaining.min(32);
        encoded.extend_from_slice(&chunk[..take]);
        if take < 32 && chunk[take..].iter().any(|byte| *byte != 0) {
            return Err(RetentionError::Source(
                "non-canonical final StorageBytes word".to_owned(),
            ));
        }
    }
    Ok(encoded)
}
