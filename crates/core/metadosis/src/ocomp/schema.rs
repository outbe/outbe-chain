use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_ocomp_protocol::{
    codec::CodecLimits,
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::{JobIntentV1, PreAdmissionEnvelopeV1},
    profile::CapacityProfileV1,
    receipts::RequestBudgetSplitReceiptV1,
    state::{OcompJobRecordV1, OcompJobStatus, OcompJobTerminalV1, OcompTerminalOutcome},
    SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_promislimit::{ocomp_budget::CarryOverCredit, schema::PromisLimitContract};

use crate::constants::MAX_RECORDS_KEPT;
use crate::schema::{status, MetadosisContract, WorldwideDayEntryExt};

use super::state::{
    ExpiredAttempt, JobFsmCommand, JobFsmLimits, JobFsmSnapshot, JobFsmState, LiveAttemptSnapshot,
    ReadyAttemptSnapshot, RetainedRequestEffectSnapshot,
};

const SCHEDULER_MAGIC: [u8; 4] = *b"OMJS";
const SCHEDULER_VERSION: u16 = 1;
const SCHEDULER_PHASE_READY: u8 = 1;
const SCHEDULER_PHASE_PENDING: u8 = 2;
const SCHEDULER_ENCODED_LEN: usize = 4 + 2 + 1 + 4 + 8 + 8 + 32 + 8 + 8 + 1 + 8 + 32 + 32;
const READY_INDEX_MAGIC: [u8; 4] = *b"OMRI";
const READY_INDEX_VERSION: u16 = 1;
const READY_INDEX_HEADER_LEN: usize = 4 + 2 + 2;
const READY_INDEX_KEY_LEN: usize = 8 + 4 + 8;
const REQUEST_PROFILE_MAGIC: [u8; 4] = *b"OMRP";
const REQUEST_PROFILE_VERSION: u16 = 1;
const REQUEST_PROFILE_FIXED_LEN: usize = 4 + 2 + 8 + 32 * 6 + 4;

/// Fork-installed authority needed to assemble `JobIntentV1`.
///
/// It deliberately carries an already-derived bundle hash rather than
/// pretending that the measurement-only OCM-04 shape manifest is armable.
/// OCM-26 supplies this value from the final network bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompRequestProfile {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub fork_id: B256,
    pub protocol_bundle_hash: B256,
    pub correctness_profile_id: B256,
    pub capacity_profile: CapacityProfileV1,
    pub source_availability_policy_id: B256,
    pub result_committee_snapshot_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OcompExpiryDisposition {
    RetryScheduled {
        next_pending_nonce: u64,
    },
    TerminalNoRetry {
        next_pending_nonce: u64,
        carry_over: CarryOverCredit,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReadyIndexKey {
    next_check_height: u64,
    worldwide_day: WorldwideDay,
    pending_nonce: u64,
}

impl ReadyIndexKey {
    fn from_projection(projection: super::state::JobFsmProjection) -> Result<Self> {
        if projection.phase != super::state::DayPhase::Ready
            || projection.live_intent_id.is_some()
            || projection.deadline_height.is_some()
        {
            return Err(fatal("OCOMP READY index received a non-READY FSM state"));
        }
        Ok(Self {
            next_check_height: projection
                .next_check_height
                .ok_or_else(|| fatal("OCOMP READY state has no due height"))?,
            worldwide_day: projection.worldwide_day,
            pending_nonce: projection.pending_nonce,
        })
    }
}

/// Generated measurement ceilings used to bound canonical request objects.
///
/// These are compile ceilings only. They do not arm a network or provide a
/// `ProtocolBundleHash`; OCM-26 supplies the final network profile.
#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
    let max_body_bytes = usize::try_from(candidate.max_activation_ocb1_bytes)
        .expect("generated body cap fits usize");
    let max_collection_items =
        usize::try_from(candidate.max_ce_mutations).expect("generated item cap fits usize");
    SchemaLimits {
        codec: CodecLimits::new(
            max_body_bytes,
            max_collection_items,
            usize::try_from(candidate.max_transaction_rlp_bytes)
                .expect("generated allocation cap fits usize"),
        ),
        max_bounded_bytes: max_body_bytes,
        max_proof_bytes: usize::try_from(candidate.max_finalized_intent_proof_bytes)
            .expect("generated proof cap fits usize"),
        max_collection_items,
        max_action_items: max_collection_items,
        max_chunk_items: usize::try_from(candidate.max_input_chunks)
            .expect("generated chunk cap fits usize"),
        max_unit_inputs: usize::try_from(candidate.max_total_units)
            .expect("generated unit cap fits usize"),
        max_control_body_bytes: usize::try_from(candidate.max_activation_payload_bytes)
            .expect("generated control cap fits usize"),
    }
}

impl MetadosisContract<'_> {
    /// Installs the exact request profile once. Repeating the same profile is
    /// idempotent; replacing any identity or cap is rejected.
    pub fn initialize_ocomp_request_profile(
        &mut self,
        profile: &OcompRequestProfile,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            validate_request_profile(profile)?;
            if storage.chain_id()? != profile.chain_id {
                return Err(fatal("OCOMP request profile chain id mismatch"));
            }
            match self.read_ocomp_request_profile(limits)? {
                Some(existing) if existing == *profile => Ok(()),
                Some(_) => Err(fatal("OCOMP request profile is immutable")),
                None => {
                    let encoded = encode_request_profile(profile, limits)?;
                    self.ocomp_request_profile.write(&encoded)?;
                    if self.read_ocomp_request_profile(limits)? != Some(profile.clone()) {
                        return Err(fatal("OCOMP request profile write/read mismatch"));
                    }
                    Ok(())
                }
            }
        })
    }

    pub fn read_ocomp_request_profile(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompRequestProfile>> {
        let len = self.ocomp_request_profile.len()?;
        if len == 0 {
            return Ok(None);
        }
        let max = REQUEST_PROFILE_FIXED_LEN
            .checked_add(limits.codec.max_body_bytes)
            .and_then(|value| value.checked_add(outbe_ocomp_protocol::OCB1_HEADER_LEN))
            .ok_or_else(|| fatal("OCOMP request profile byte cap overflow"))?;
        if len > max {
            return Err(fatal("OCOMP request profile exceeds byte cap"));
        }
        decode_request_profile(&self.ocomp_request_profile.read()?, limits).map(Some)
    }

    /// Inserts one bounded ordered READY key without scanning active
    /// WorldwideDays. Repeating the exact enqueue is idempotent.
    pub(crate) fn enqueue_ocomp_ready(
        &mut self,
        wwd: WorldwideDay,
        next_check_height: u64,
        limits: JobFsmLimits,
    ) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let status_value = self.worldwide_days.entry(wwd).status().read()?;
            if status_value != status::READY {
                return Err(fatal(format!(
                    "cannot enqueue OCOMP WWD {wwd} from status {status_value}"
                )));
            }
            if self.ocomp_fsm_states.get_bytes(&wwd).is_empty()? {
                let state = JobFsmState::initial_ready(wwd, next_check_height);
                state
                    .validate(limits)
                    .map_err(|error| fatal(error.to_string()))?;
                let key = ReadyIndexKey::from_projection(state.projection())?;
                let mut index = self.read_ready_index()?;
                insert_ready_key(&mut index, key)?;
                self.write_ocomp_state(&state)?;
                return self.write_ready_index(&index);
            }

            let existing = self.ocomp_fsm_state(wwd, &poc_schema_limits(), limits)?;
            let projection = existing.projection();
            if projection.phase != super::state::DayPhase::Ready {
                return Err(fatal("cannot enqueue a WWD while its OCOMP intent is live"));
            }
            let key = ReadyIndexKey::from_projection(projection)?;
            if key.next_check_height != next_check_height {
                return Err(fatal("idempotent OCOMP enqueue changed the due height"));
            }
            if self.read_ready_index()?.binary_search(&key).is_err() {
                return Err(fatal("OCOMP READY state has no exact due-index key"));
            }
            Ok(())
        })
    }

    /// Moves only the READY due key after a deterministic deferred admission.
    pub(crate) fn defer_ocomp_ready(
        &mut self,
        wwd: WorldwideDay,
        at_height: u64,
        next_check_height: u64,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let mut state = self.ocomp_fsm_state(wwd, schema_limits, fsm_limits)?;
            let old_key = ReadyIndexKey::from_projection(state.projection())?;
            state
                .apply(
                    JobFsmCommand::Defer {
                        at_height,
                        next_check_height,
                    },
                    fsm_limits,
                )
                .map_err(|error| fatal(error.to_string()))?;
            let new_key = ReadyIndexKey::from_projection(state.projection())?;
            let mut index = self.read_ready_index()?;
            remove_ready_key(&mut index, old_key)?;
            insert_ready_key(&mut index, new_key)?;
            self.write_ocomp_state(&state)?;
            self.write_ready_index(&index)
        })
    }

    /// Returns only the first exact READY key in canonical due order.
    ///
    /// Terminal request processing calls this once per block; it never scans
    /// the active WorldwideDay set.
    pub(crate) fn next_ocomp_ready(
        &self,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<Option<super::state::JobFsmProjection>> {
        let Some(key) = self.read_ready_index()?.first().copied() else {
            return Ok(None);
        };
        let projection = self
            .ocomp_fsm_state(key.worldwide_day, schema_limits, fsm_limits)?
            .projection();
        if ReadyIndexKey::from_projection(projection)? != key {
            return Err(fatal("OCOMP READY index/FSM key mismatch"));
        }
        Ok(Some(projection))
    }

    /// Commits the exact owner-projection envelope once and retains its
    /// canonical bytes for finalized job reconstruction. An exact retry is
    /// idempotent; any different envelope is invariant corruption.
    pub(crate) fn commit_pre_admission_envelope(
        &mut self,
        wwd: WorldwideDay,
        envelope: &PreAdmissionEnvelopeV1,
        limits: &SchemaLimits,
    ) -> Result<crate::MetadosisPreAdmissionProjection> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let encoded = envelope
                .encode_canonical(limits)
                .map_err(|error| fatal(format!("encode OCOMP pre-admission envelope: {error}")))?;
            let expected_hash = envelope
                .envelope_hash(limits)
                .map_err(|error| fatal(format!("hash OCOMP pre-admission envelope: {error}")))?;
            let existing = self.read_pre_admission_envelope(wwd, limits)?;
            let projection = self.ocomp_pre_admission_projection(wwd)?;

            match (existing, projection.envelope_hash.is_zero()) {
                (None, true) => {
                    let sealed = self.seal_pre_admission_envelope(wwd, envelope, limits)?;
                    if sealed.envelope_hash != expected_hash {
                        return Err(fatal("OCOMP pre-admission seal/hash mismatch"));
                    }
                    self.ocomp_pre_admission_envelopes
                        .get_bytes(&wwd)
                        .write(&encoded)?;
                    if self.read_pre_admission_envelope(wwd, limits)?.as_ref() != Some(envelope) {
                        return Err(fatal("OCOMP pre-admission envelope write/read mismatch"));
                    }
                    Ok(sealed)
                }
                (Some(existing), false)
                    if existing == *envelope && projection.envelope_hash == expected_hash =>
                {
                    Ok(projection)
                }
                _ => Err(fatal("immutable OCOMP pre-admission envelope changed")),
            }
        })
    }

    pub(crate) fn read_pre_admission_envelope(
        &self,
        wwd: WorldwideDay,
        limits: &SchemaLimits,
    ) -> Result<Option<PreAdmissionEnvelopeV1>> {
        let bytes = self.ocomp_pre_admission_envelopes.get_bytes(&wwd);
        read_canonical_optional(
            &bytes,
            max_canonical_object_bytes(limits)?,
            |encoded| PreAdmissionEnvelopeV1::decode_canonical(encoded, limits),
            "OCOMP pre-admission envelope",
        )
    }

    /// Commits one canonical live job and all Metadosis-owned indexes.
    ///
    /// The caller has already applied or replay-validated the owner budget
    /// effect. This method nevertheless requires and stores the exact receipt,
    /// closing the persisted receipt/state equivalence.
    pub(crate) fn commit_ocomp_request(
        &mut self,
        intent: &JobIntentV1,
        receipt: &RequestBudgetSplitReceiptV1,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            intent
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid OCOMP intent: {error}")))?;
            receipt
                .validate_semantics()
                .map_err(|error| fatal(format!("invalid OCOMP request receipt: {error}")))?;
            let wwd = WorldwideDay::new(intent.wwd);
            if self.worldwide_days.entry(wwd).status().read()? != status::READY {
                return Err(fatal("OCOMP request requires READY WorldwideDay"));
            }

            let receipt_hash = receipt
                .receipt_hash(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP request receipt: {error}")))?;
            if receipt_hash
                != intent
                    .frozen_metadosis_values
                    .request_budget_split_receipt_hash
                || receipt.wwd != intent.wwd
                || receipt.pending_nonce > intent.pending_nonce
                || receipt.protocol_bundle_hash != intent.protocol_bundle_hash
                || receipt.lysis_budget != intent.frozen_metadosis_values.lysis_budget
            {
                return Err(fatal("OCOMP intent/request receipt binding mismatch"));
            }

            if !self.ocomp_scheduler.is_empty()? {
                return Err(fatal("PoC permits only one live OCOMP intent"));
            }
            let mut state = self.ocomp_fsm_state(wwd, schema_limits, fsm_limits)?;
            let ready_key = ReadyIndexKey::from_projection(state.projection())?;
            let existing_receipt = self.request_budget_receipt(wwd, schema_limits)?;
            if matches!(existing_receipt, Some(ref existing) if existing != receipt) {
                return Err(fatal("immutable OCOMP request receipt changed"));
            }
            let intent_id = intent
                .intent_id(schema_limits)
                .map_err(|error| fatal(format!("hash OCOMP intent: {error}")))?;
            if !self.ocomp_job_records.get_bytes(&intent_id).is_empty()? {
                return Err(fatal("OCOMP IntentId already has a job record"));
            }
            state
                .apply(
                    JobFsmCommand::Request {
                        at_height: intent.logical_evaluation_height,
                        intent_id,
                        deadline_height: intent.deadline_height,
                        lysis_budget: intent.frozen_metadosis_values.lysis_budget,
                        request_budget_receipt_hash: receipt_hash,
                    },
                    fsm_limits,
                )
                .map_err(|error| fatal(error.to_string()))?;

            if existing_receipt.is_none() {
                self.ocomp_request_budget_receipts.get_bytes(&wwd).write(
                    &receipt
                        .encode_canonical(schema_limits)
                        .map_err(|error| fatal(format!("encode request receipt: {error}")))?,
                )?;
            }
            let record = OcompJobRecordV1 {
                intent: intent.clone(),
                status: OcompJobStatus::OffchainPending,
                terminal: None,
            };
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.worldwide_days
                .entry(wwd)
                .status()
                .write(status::OFFCHAIN_PENDING)?;
            let mut ready_index = self.read_ready_index()?;
            remove_ready_key(&mut ready_index, ready_key)?;
            self.write_ready_index(&ready_index)?;
            self.write_ocomp_state(&state)?;
            self.write_live_scheduler(&state)
        })
    }

    /// Applies the exclusive begin-zone expiry to the exact live index.
    pub(crate) fn expire_ocomp_job(
        &mut self,
        at_height: u64,
        at_time: u64,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<OcompExpiryDisposition> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let mut state = self
                .live_ocomp_fsm_state(schema_limits, fsm_limits)?
                .ok_or_else(|| fatal("OCOMP expiry index is empty"))?;
            let live_intent_id = state
                .projection()
                .live_intent_id
                .ok_or_else(|| fatal("OCOMP expiry index has no live intent"))?;
            let mut record = self
                .ocomp_job_record(live_intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP live index points to a missing job"))?;
            let before_terminal = state.projection().terminal_records;
            state
                .apply(JobFsmCommand::Expire { at_height, at_time }, fsm_limits)
                .map_err(|error| fatal(error.to_string()))?;
            let terminal = state
                .expired_attempts()
                .last()
                .copied()
                .ok_or_else(|| fatal("OCOMP expiry produced no terminal evidence"))?;
            if terminal.intent_id != live_intent_id
                || state.projection().terminal_records != before_terminal.saturating_add(1)
            {
                return Err(fatal("OCOMP terminal index/count mismatch"));
            }
            if self.ocomp_terminal_intents.len()? >= u32::from(fsm_limits.max_terminal_records) {
                return Err(fatal("OCOMP terminal record cap exhausted"));
            }
            let projection = state.projection();
            let next_pending_nonce = terminal.next_pending_nonce;
            let retained_lysis_budget = projection
                .retained_lysis_budget
                .ok_or_else(|| fatal("expired OCOMP job has no retained Lysis budget"))?;
            if retained_lysis_budget != record.intent.frozen_metadosis_values.lysis_budget {
                return Err(fatal("expired OCOMP job retained budget mismatch"));
            }

            record.status = OcompJobStatus::Expired;
            record.terminal = Some(OcompJobTerminalV1 {
                outcome: OcompTerminalOutcome::Expired,
                terminal_height: terminal.terminal_height,
                terminal_time: terminal.terminal_time,
                next_pending_nonce: Some(terminal.next_pending_nonce),
                completed_binding: None,
            });
            self.write_ocomp_job_record(live_intent_id, &record, schema_limits)?;
            self.ocomp_terminal_intents.push(live_intent_id)?;
            let wwd = WorldwideDay::new(record.intent.wwd);

            if projection.terminal_records == fsm_limits.max_terminal_records {
                if self
                    .read_ready_index()?
                    .iter()
                    .any(|key| key.worldwide_day == wwd)
                {
                    return Err(fatal(
                        "terminal no-retry WWD unexpectedly remains in READY index",
                    ));
                }
                let carry_over = PromisLimitContract::new(self.storage.clone())
                    .checked_add_carry_over(retained_lysis_budget)?;
                self.mark_wwd_failed(wwd)?;
                self.ocomp_fsm_states.get_bytes(&wwd).clear()?;
                self.ocomp_scheduler.clear()?;
                return Ok(OcompExpiryDisposition::TerminalNoRetry {
                    next_pending_nonce,
                    carry_over,
                });
            }

            self.worldwide_days
                .entry(wwd)
                .status()
                .write(status::READY)?;
            let ready_key = ReadyIndexKey::from_projection(projection)?;
            let mut ready_index = self.read_ready_index()?;
            insert_ready_key(&mut ready_index, ready_key)?;
            self.write_ocomp_state(&state)?;
            self.write_ready_index(&ready_index)?;
            self.ocomp_scheduler.clear()?;
            Ok(OcompExpiryDisposition::RetryScheduled { next_pending_nonce })
        })
    }

    pub(crate) fn request_budget_receipt(
        &self,
        wwd: WorldwideDay,
        limits: &SchemaLimits,
    ) -> Result<Option<RequestBudgetSplitReceiptV1>> {
        let bytes = self.ocomp_request_budget_receipts.get_bytes(&wwd);
        read_canonical_optional(
            &bytes,
            max_canonical_object_bytes(limits)?,
            |encoded| RequestBudgetSplitReceiptV1::decode_canonical(encoded, limits),
            "OCOMP request budget receipt",
        )
    }

    pub fn ocomp_job_record(
        &self,
        intent_id: B256,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompJobRecordV1>> {
        let bytes = self.ocomp_job_records.get_bytes(&intent_id);
        let record = read_canonical_optional(
            &bytes,
            max_canonical_object_bytes(limits)?,
            |encoded| OcompJobRecordV1::decode_canonical(encoded, limits),
            "OCOMP job record",
        )?;
        if let Some(record) = &record {
            let actual = record
                .intent
                .intent_id(limits)
                .map_err(|error| fatal(format!("hash stored OCOMP intent: {error}")))?;
            if actual != intent_id {
                return Err(fatal("OCOMP job record key/IntentId mismatch"));
            }
        }
        Ok(record)
    }

    pub(crate) fn latest_terminal_job_record(
        &self,
        wwd: WorldwideDay,
        limits: &SchemaLimits,
    ) -> Result<Option<(B256, OcompJobRecordV1)>> {
        let mut index = self.ocomp_terminal_intents.len()?;
        while index > 0 {
            index -= 1;
            let intent_id = self
                .ocomp_terminal_intents
                .get(index)?
                .ok_or_else(|| fatal("OCOMP terminal index is sparse"))?;
            let record = self
                .ocomp_job_record(intent_id, limits)?
                .ok_or_else(|| fatal("OCOMP terminal index points to a missing job"))?;
            if record.intent.wwd == wwd.value() {
                return Ok(Some((intent_id, record)));
            }
        }
        Ok(None)
    }

    pub(crate) fn ocomp_fsm_state(
        &self,
        wwd: WorldwideDay,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<JobFsmState> {
        let encoded = self.ocomp_fsm_states.get_bytes(&wwd).read()?;
        if encoded.is_empty() {
            return Err(fatal("OCOMP WWD FSM is not initialized"));
        }
        let mut snapshot = decode_scheduler(&encoded)?;
        if snapshot.worldwide_day != wwd {
            return Err(fatal("OCOMP WWD FSM storage key mismatch"));
        }

        let terminal_count = self.ocomp_terminal_intents.len()?;
        if terminal_count > u32::from(fsm_limits.max_terminal_records) {
            return Err(fatal("OCOMP terminal index exceeds profile cap"));
        }
        let terminal_ids = self.ocomp_terminal_intents.read_all()?;
        let mut expired = Vec::new();
        expired
            .try_reserve_exact(terminal_ids.len())
            .map_err(|_| fatal("allocate bounded OCOMP terminal evidence"))?;
        for intent_id in terminal_ids {
            let record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| fatal("OCOMP terminal index points to a missing job"))?;
            let terminal = record
                .terminal
                .as_ref()
                .ok_or_else(|| fatal("OCOMP terminal job has no terminal evidence"))?;
            if record.status != OcompJobStatus::Expired
                || terminal.outcome != OcompTerminalOutcome::Expired
                || terminal.completed_binding.is_some()
            {
                return Err(fatal("OCOMP terminal index points to a non-expired job"));
            }
            if record.intent.wwd != wwd.value() {
                continue;
            }
            let next_pending_nonce = terminal
                .next_pending_nonce
                .ok_or_else(|| fatal("expired OCOMP job lacks next nonce"))?;
            expired.push(ExpiredAttempt {
                intent_id,
                pending_nonce: record.intent.pending_nonce,
                terminal_height: terminal.terminal_height,
                terminal_time: terminal.terminal_time,
                next_pending_nonce,
            });
        }
        snapshot.expired = expired;

        let state = JobFsmState::restore(snapshot, fsm_limits)
            .map_err(|error| fatal(format!("restore OCOMP FSM: {error}")))?;
        self.validate_persisted_equivalences(&state, schema_limits)?;
        Ok(state)
    }

    pub(crate) fn live_ocomp_fsm_state(
        &self,
        schema_limits: &SchemaLimits,
        fsm_limits: JobFsmLimits,
    ) -> Result<Option<JobFsmState>> {
        let encoded = self.ocomp_scheduler.read()?;
        if encoded.is_empty() {
            return Ok(None);
        }
        let snapshot = decode_scheduler(&encoded)?;
        let state = self.ocomp_fsm_state(snapshot.worldwide_day, schema_limits, fsm_limits)?;
        if state.projection().phase != super::state::DayPhase::OffchainPending
            || encode_scheduler(&state)? != encoded
        {
            return Err(fatal("OCOMP live index/FSM mismatch"));
        }
        Ok(Some(state))
    }

    fn validate_persisted_equivalences(
        &self,
        state: &JobFsmState,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let projection = state.projection();
        let expected_status = match projection.phase {
            super::state::DayPhase::Ready => status::READY,
            super::state::DayPhase::OffchainPending => status::OFFCHAIN_PENDING,
        };
        if self
            .worldwide_days
            .entry(projection.worldwide_day)
            .status()
            .read()?
            != expected_status
        {
            return Err(fatal("OCOMP scheduler/WorldwideDay status mismatch"));
        }

        let receipt = self.request_budget_receipt(projection.worldwide_day, limits)?;
        match projection.retained_lysis_budget {
            None if receipt.is_none() && projection.pending_nonce == 0 => {}
            Some(lysis_budget) => {
                let receipt =
                    receipt.ok_or_else(|| fatal("OCOMP retained budget has no receipt"))?;
                let expected_hash = receipt
                    .receipt_hash(limits)
                    .map_err(|error| fatal(format!("hash stored request receipt: {error}")))?;
                let snapshot = state.snapshot();
                let retained = snapshot
                    .ready
                    .and_then(|ready| ready.retained_effect)
                    .or_else(|| snapshot.live.map(|live| live.retained_effect))
                    .ok_or_else(|| fatal("OCOMP retained effect snapshot is missing"))?;
                if receipt.wwd != projection.worldwide_day.value()
                    || receipt.lysis_budget != lysis_budget
                    || receipt.pending_nonce != retained.effect_nonce
                    || expected_hash != retained.receipt_hash
                {
                    return Err(fatal("OCOMP budget receipt/state mismatch"));
                }
            }
            _ => return Err(fatal("OCOMP fresh READY state has a residual receipt")),
        }

        match projection.phase {
            super::state::DayPhase::Ready => {
                let key = ReadyIndexKey::from_projection(projection)?;
                if self.read_ready_index()?.binary_search(&key).is_err() {
                    return Err(fatal("OCOMP READY FSM has no exact due-index key"));
                }
            }
            super::state::DayPhase::OffchainPending => {
                let intent_id = projection
                    .live_intent_id
                    .ok_or_else(|| fatal("OCOMP pending FSM has no live intent"))?;
                let record = self
                    .ocomp_job_record(intent_id, limits)?
                    .ok_or_else(|| fatal("OCOMP live scheduler key has no job record"))?;
                if record.status != OcompJobStatus::OffchainPending
                    || record.terminal.is_some()
                    || record.intent.wwd != projection.worldwide_day.value()
                    || record.intent.pending_nonce != projection.pending_nonce
                    || record.intent.deadline_height
                        != projection.deadline_height.unwrap_or_default()
                {
                    return Err(fatal("OCOMP live scheduler/job record mismatch"));
                }
                if self
                    .read_ready_index()?
                    .iter()
                    .any(|key| key.worldwide_day == projection.worldwide_day)
                {
                    return Err(fatal("OCOMP live WWD remains in the READY index"));
                }
            }
        }
        Ok(())
    }

    fn write_ocomp_job_record(
        &self,
        intent_id: B256,
        record: &OcompJobRecordV1,
        limits: &SchemaLimits,
    ) -> Result<()> {
        record
            .validate_semantics(limits)
            .map_err(|error| fatal(format!("invalid OCOMP job record: {error}")))?;
        let encoded = record
            .encode_canonical(limits)
            .map_err(|error| fatal(format!("encode OCOMP job record: {error}")))?;
        self.ocomp_job_records.get_bytes(&intent_id).write(&encoded)
    }

    fn write_ocomp_state(&self, state: &JobFsmState) -> Result<()> {
        self.ocomp_fsm_states
            .get_bytes(&state.projection().worldwide_day)
            .write(&encode_scheduler(state)?)
    }

    fn write_live_scheduler(&self, state: &JobFsmState) -> Result<()> {
        if state.projection().phase != super::state::DayPhase::OffchainPending {
            return Err(fatal("OCOMP live scheduler requires pending state"));
        }
        self.ocomp_scheduler.write(&encode_scheduler(state)?)
    }

    fn read_ready_index(&self) -> Result<Vec<ReadyIndexKey>> {
        decode_ready_index(&self.ocomp_ready_index.read()?)
    }

    fn write_ready_index(&self, index: &[ReadyIndexKey]) -> Result<()> {
        if index.is_empty() {
            return self.ocomp_ready_index.clear();
        }
        self.ocomp_ready_index.write(&encode_ready_index(index)?)
    }
}

fn read_canonical_optional<T>(
    bytes: &outbe_primitives::storage::types::StorageBytes<'_>,
    max_encoded_bytes: usize,
    decode: impl FnOnce(&[u8]) -> core::result::Result<T, outbe_ocomp_protocol::ProtocolError>,
    label: &'static str,
) -> Result<Option<T>> {
    let len = bytes.len()?;
    if len == 0 {
        return Ok(None);
    }
    if len > max_encoded_bytes {
        return Err(fatal(format!("{label} exceeds canonical byte cap")));
    }
    let encoded = bytes.read()?;
    decode(&encoded)
        .map(Some)
        .map_err(|error| fatal(format!("decode {label}: {error}")))
}

fn insert_ready_key(index: &mut Vec<ReadyIndexKey>, key: ReadyIndexKey) -> Result<()> {
    if index
        .iter()
        .any(|existing| existing.worldwide_day == key.worldwide_day)
    {
        return Err(fatal("OCOMP READY index already contains the WWD"));
    }
    if index.len() >= MAX_RECORDS_KEPT {
        return Err(fatal("OCOMP READY index capacity exhausted"));
    }
    let position = index
        .binary_search(&key)
        .unwrap_or_else(|position| position);
    index.insert(position, key);
    validate_ready_index(index)
}

fn remove_ready_key(index: &mut Vec<ReadyIndexKey>, key: ReadyIndexKey) -> Result<()> {
    let position = index
        .binary_search(&key)
        .map_err(|_| fatal("OCOMP READY index is missing the exact FSM key"))?;
    index.remove(position);
    validate_ready_index(index)
}

fn encode_ready_index(index: &[ReadyIndexKey]) -> Result<Vec<u8>> {
    validate_ready_index(index)?;
    let count =
        u16::try_from(index.len()).map_err(|_| fatal("OCOMP READY index count exceeds u16"))?;
    let capacity = READY_INDEX_KEY_LEN
        .checked_mul(index.len())
        .and_then(|bytes| READY_INDEX_HEADER_LEN.checked_add(bytes))
        .ok_or_else(|| fatal("OCOMP READY index encoded length overflow"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(capacity)
        .map_err(|_| fatal("allocate bounded OCOMP READY index"))?;
    encoded.extend_from_slice(&READY_INDEX_MAGIC);
    encoded.extend_from_slice(&READY_INDEX_VERSION.to_be_bytes());
    encoded.extend_from_slice(&count.to_be_bytes());
    for key in index {
        encoded.extend_from_slice(&key.next_check_height.to_be_bytes());
        encoded.extend_from_slice(&key.worldwide_day.value().to_be_bytes());
        encoded.extend_from_slice(&key.pending_nonce.to_be_bytes());
    }
    if encoded.len() != capacity {
        return Err(fatal("OCOMP READY index encoded length mismatch"));
    }
    Ok(encoded)
}

fn decode_ready_index(encoded: &[u8]) -> Result<Vec<ReadyIndexKey>> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    if encoded.len() < READY_INDEX_HEADER_LEN {
        return Err(fatal("OCOMP READY index is shorter than its header"));
    }
    let mut reader = FixedReader::new(encoded);
    if reader.take::<4>()? != READY_INDEX_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != READY_INDEX_VERSION
    {
        return Err(fatal("OCOMP READY index magic/version mismatch"));
    }
    let count = usize::from(u16::from_be_bytes(reader.take::<2>()?));
    if count > MAX_RECORDS_KEPT {
        return Err(fatal("OCOMP READY index exceeds its fixed capacity"));
    }
    let expected_len = READY_INDEX_KEY_LEN
        .checked_mul(count)
        .and_then(|bytes| READY_INDEX_HEADER_LEN.checked_add(bytes))
        .ok_or_else(|| fatal("OCOMP READY index declared length overflow"))?;
    if encoded.len() != expected_len {
        return Err(fatal("OCOMP READY index has non-canonical length"));
    }
    let mut index = Vec::new();
    index
        .try_reserve_exact(count)
        .map_err(|_| fatal("allocate bounded OCOMP READY index"))?;
    for _ in 0..count {
        let key = ReadyIndexKey {
            next_check_height: reader.u64()?,
            worldwide_day: WorldwideDay::new(reader.u32()?),
            pending_nonce: reader.u64()?,
        };
        index.push(key);
    }
    reader.finish()?;
    validate_ready_index(&index)?;
    Ok(index)
}

fn validate_ready_index(index: &[ReadyIndexKey]) -> Result<()> {
    if index.len() > MAX_RECORDS_KEPT {
        return Err(fatal("OCOMP READY index exceeds its fixed capacity"));
    }
    if index.iter().any(|key| !key.worldwide_day.is_valid()) {
        return Err(fatal("OCOMP READY index contains an invalid WorldwideDay"));
    }
    if index.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(fatal("OCOMP READY index is not in strict canonical order"));
    }
    for (position, key) in index.iter().enumerate() {
        if index[..position]
            .iter()
            .any(|existing| existing.worldwide_day == key.worldwide_day)
        {
            return Err(fatal("OCOMP READY index contains a duplicate WWD"));
        }
    }
    Ok(())
}

fn encode_scheduler(state: &JobFsmState) -> Result<Vec<u8>> {
    let snapshot = state.snapshot();
    let mut encoded = Vec::with_capacity(SCHEDULER_ENCODED_LEN);
    encoded.extend_from_slice(&SCHEDULER_MAGIC);
    encoded.extend_from_slice(&SCHEDULER_VERSION.to_be_bytes());
    match (snapshot.ready, snapshot.live) {
        (Some(ready), None) => {
            encoded.push(SCHEDULER_PHASE_READY);
            encoded.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
            encoded.extend_from_slice(&ready.pending_nonce.to_be_bytes());
            encoded.extend_from_slice(&ready.next_check_height.to_be_bytes());
            encoded.extend_from_slice(B256::ZERO.as_slice());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encode_retained_effect(&mut encoded, ready.retained_effect);
        }
        (None, Some(live)) => {
            encoded.push(SCHEDULER_PHASE_PENDING);
            encoded.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
            encoded.extend_from_slice(&live.pending_nonce.to_be_bytes());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(live.intent_id.as_slice());
            encoded.extend_from_slice(&live.requested_height.to_be_bytes());
            encoded.extend_from_slice(&live.deadline_height.to_be_bytes());
            encode_retained_effect(&mut encoded, Some(live.retained_effect));
        }
        _ => return Err(fatal("encode invalid OCOMP scheduler phase cardinality")),
    }
    if encoded.len() != SCHEDULER_ENCODED_LEN {
        return Err(fatal("OCOMP scheduler encoded length mismatch"));
    }
    Ok(encoded)
}

fn encode_retained_effect(encoded: &mut Vec<u8>, effect: Option<RetainedRequestEffectSnapshot>) {
    match effect {
        Some(effect) => {
            encoded.push(1);
            encoded.extend_from_slice(&effect.effect_nonce.to_be_bytes());
            encoded.extend_from_slice(&effect.lysis_budget.to_be_bytes::<32>());
            encoded.extend_from_slice(effect.receipt_hash.as_slice());
        }
        None => {
            encoded.push(0);
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
            encoded.extend_from_slice(B256::ZERO.as_slice());
        }
    }
}

fn decode_scheduler(encoded: &[u8]) -> Result<JobFsmSnapshot> {
    if encoded.len() != SCHEDULER_ENCODED_LEN {
        return Err(fatal("OCOMP scheduler has non-canonical length"));
    }
    let mut reader = FixedReader::new(encoded);
    if reader.take::<4>()? != SCHEDULER_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != SCHEDULER_VERSION
    {
        return Err(fatal("OCOMP scheduler magic/version mismatch"));
    }
    let phase = reader.u8()?;
    let worldwide_day = WorldwideDay::new(reader.u32()?);
    let pending_nonce = reader.u64()?;
    let next_check_height = reader.u64()?;
    let intent_id = B256::from(reader.take::<32>()?);
    let requested_height = reader.u64()?;
    let deadline_height = reader.u64()?;
    let retained_effect = decode_retained_effect(&mut reader)?;
    reader.finish()?;

    let (ready, live) = match phase {
        SCHEDULER_PHASE_READY
            if intent_id.is_zero() && requested_height == 0 && deadline_height == 0 =>
        {
            (
                Some(ReadyAttemptSnapshot {
                    pending_nonce,
                    next_check_height,
                    retained_effect,
                }),
                None,
            )
        }
        SCHEDULER_PHASE_PENDING
            if next_check_height == 0 && !intent_id.is_zero() && retained_effect.is_some() =>
        {
            (
                None,
                Some(LiveAttemptSnapshot {
                    intent_id,
                    pending_nonce,
                    requested_height,
                    deadline_height,
                    retained_effect: retained_effect
                        .ok_or_else(|| fatal("pending OCOMP scheduler has no retained effect"))?,
                }),
            )
        }
        _ => return Err(fatal("OCOMP scheduler phase/index fields are inconsistent")),
    };
    Ok(JobFsmSnapshot {
        worldwide_day,
        ready,
        live,
        expired: Vec::new(),
    })
}

fn decode_retained_effect(
    reader: &mut FixedReader<'_>,
) -> Result<Option<RetainedRequestEffectSnapshot>> {
    let present = reader.u8()?;
    let effect_nonce = reader.u64()?;
    let lysis_budget = U256::from_be_bytes(reader.take::<32>()?);
    let receipt_hash = B256::from(reader.take::<32>()?);
    match present {
        0 if effect_nonce == 0 && lysis_budget.is_zero() && receipt_hash.is_zero() => Ok(None),
        1 if !receipt_hash.is_zero() => Ok(Some(RetainedRequestEffectSnapshot {
            effect_nonce,
            lysis_budget,
            receipt_hash,
        })),
        _ => Err(fatal(
            "OCOMP scheduler retained-effect fields are inconsistent",
        )),
    }
}

fn validate_request_profile(profile: &OcompRequestProfile) -> Result<()> {
    let capacity = &profile.capacity_profile;
    if profile.chain_id == 0
        || profile.genesis_hash.is_zero()
        || profile.fork_id.is_zero()
        || profile.protocol_bundle_hash.is_zero()
        || profile.correctness_profile_id.is_zero()
        || profile.source_availability_policy_id.is_zero()
        || profile.result_committee_snapshot_hash.is_zero()
        || capacity.profile_id.is_zero()
        || capacity.generated_limits_manifest_hash.is_zero()
    {
        return Err(fatal(
            "OCOMP request profile contains a reserved zero identity",
        ));
    }

    let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
    let max_poc_tributes = u32::try_from(candidate.max_poc_tributes)
        .map_err(|_| fatal("generated Tribute cap exceeds u32"))?;
    let unit_tributes = u32::try_from(candidate.unit_tributes)
        .map_err(|_| fatal("generated unit size exceeds u32"))?;
    let max_reference_currencies = u16::try_from(candidate.max_oracle_openings)
        .map_err(|_| fatal("generated reference-currency cap exceeds u16"))?;
    let max_oracle_entries = u32::try_from(candidate.max_oracle_wwd_pair_entries)
        .map_err(|_| fatal("generated Oracle entry cap exceeds u32"))?;
    let max_active_scurve_entries = u32::try_from(candidate.max_active_scurve_entries)
        .map_err(|_| fatal("generated S-curve entry cap exceeds u32"))?;

    if capacity.max_poc_tributes == 0
        || capacity.max_poc_tributes > max_poc_tributes
        || capacity.unit_tributes != unit_tributes
        || capacity.unit_tributes > capacity.max_poc_tributes
        || capacity.max_workers_per_domain != 4
        || capacity.max_pending_jobs != 1
        || capacity.max_intents_per_block != 1
        || capacity.max_activations_per_block != 1
        || capacity.max_ready_inspections_per_block != 1
        || capacity.max_expirations_per_block != 1
        || capacity.retry_backoff_blocks != 1
        || capacity.max_terminal_job_records != 365
        || capacity.max_reference_currencies == 0
        || capacity.max_reference_currencies > max_reference_currencies
        || capacity.max_fidelity_cohorts_per_owner != 64
        || capacity.max_oracle_wwd_pair_entries == 0
        || capacity.max_oracle_wwd_pair_entries > max_oracle_entries
        || capacity.max_active_scurve_entries == 0
        || capacity.max_active_scurve_entries > max_active_scurve_entries
        || capacity.result_deadline_blocks != 64
        || capacity.source_retention_after_terminal_blocks
            != candidate.source_retention_after_terminal_blocks
    {
        return Err(fatal("OCOMP request profile violates frozen PoC bounds"));
    }
    Ok(())
}

fn encode_request_profile(profile: &OcompRequestProfile, limits: &SchemaLimits) -> Result<Vec<u8>> {
    validate_request_profile(profile)?;
    let capacity = profile
        .capacity_profile
        .encode_canonical(limits)
        .map_err(|error| fatal(format!("encode OCOMP capacity profile: {error}")))?;
    let capacity_len =
        u32::try_from(capacity.len()).map_err(|_| fatal("capacity profile length exceeds u32"))?;
    let total = REQUEST_PROFILE_FIXED_LEN
        .checked_add(capacity.len())
        .ok_or_else(|| fatal("OCOMP request profile length overflow"))?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(total)
        .map_err(|_| fatal("allocate OCOMP request profile"))?;
    encoded.extend_from_slice(&REQUEST_PROFILE_MAGIC);
    encoded.extend_from_slice(&REQUEST_PROFILE_VERSION.to_be_bytes());
    encoded.extend_from_slice(&profile.chain_id.to_be_bytes());
    encoded.extend_from_slice(profile.genesis_hash.as_slice());
    encoded.extend_from_slice(profile.fork_id.as_slice());
    encoded.extend_from_slice(profile.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(profile.correctness_profile_id.as_slice());
    encoded.extend_from_slice(profile.source_availability_policy_id.as_slice());
    encoded.extend_from_slice(profile.result_committee_snapshot_hash.as_slice());
    encoded.extend_from_slice(&capacity_len.to_be_bytes());
    encoded.extend_from_slice(&capacity);
    if encoded.len() != total {
        return Err(fatal("OCOMP request profile encoded length mismatch"));
    }
    Ok(encoded)
}

fn decode_request_profile(encoded: &[u8], limits: &SchemaLimits) -> Result<OcompRequestProfile> {
    if encoded.len() < REQUEST_PROFILE_FIXED_LEN {
        return Err(fatal("truncated OCOMP request profile"));
    }
    let mut reader = FixedReader::new(encoded);
    if reader.take::<4>()? != REQUEST_PROFILE_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != REQUEST_PROFILE_VERSION
    {
        return Err(fatal("OCOMP request profile magic/version mismatch"));
    }
    let chain_id = reader.u64()?;
    let genesis_hash = B256::from(reader.take::<32>()?);
    let fork_id = B256::from(reader.take::<32>()?);
    let protocol_bundle_hash = B256::from(reader.take::<32>()?);
    let correctness_profile_id = B256::from(reader.take::<32>()?);
    let source_availability_policy_id = B256::from(reader.take::<32>()?);
    let result_committee_snapshot_hash = B256::from(reader.take::<32>()?);
    let capacity_len = usize::try_from(reader.u32()?)
        .map_err(|_| fatal("OCOMP capacity profile length exceeds usize"))?;
    if capacity_len != reader.remaining() {
        return Err(fatal("OCOMP capacity profile length mismatch"));
    }
    let capacity_encoded = reader.take_dynamic(capacity_len)?;
    reader.finish()?;
    let capacity_profile = CapacityProfileV1::decode_canonical(capacity_encoded, limits)
        .map_err(|error| fatal(format!("decode OCOMP capacity profile: {error}")))?;
    let profile = OcompRequestProfile {
        chain_id,
        genesis_hash,
        fork_id,
        protocol_bundle_hash,
        correctness_profile_id,
        capacity_profile,
        source_availability_policy_id,
        result_committee_snapshot_hash,
    };
    validate_request_profile(&profile)?;
    if encode_request_profile(&profile, limits)? != encoded {
        return Err(fatal("non-canonical OCOMP request profile encoding"));
    }
    Ok(profile)
}

struct FixedReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> FixedReader<'a> {
    const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| fatal("OCOMP scheduler offset overflow"))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP scheduler"))?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| fatal("invalid OCOMP scheduler field width"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take::<4>()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take::<8>()?))
    }

    fn remaining(&self) -> usize {
        self.encoded.len().saturating_sub(self.offset)
    }

    fn take_dynamic(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| fatal("OCOMP request profile offset overflow"))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(|| fatal("truncated OCOMP request profile field"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn finish(self) -> Result<()> {
        if self.offset == self.encoded.len() {
            Ok(())
        } else {
            Err(fatal("OCOMP scheduler has trailing bytes"))
        }
    }
}

fn max_canonical_object_bytes(limits: &SchemaLimits) -> Result<usize> {
    limits
        .codec
        .max_body_bytes
        .checked_add(outbe_ocomp_protocol::OCB1_HEADER_LEN)
        .ok_or_else(|| fatal("OCOMP canonical object byte cap overflow"))
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
