use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    intent::intent_storage_key,
    receipts::RequestLimitSplitReceiptV1,
    state::{ActiveGenerationV1, OcompJobRecordV1, OcompJobStatus},
    vote::OcompVoteAccountabilityV1,
    SchemaLimits,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::{
    aggregate::WwdStatus,
    errors::storage_corruption_message,
    schema::{MetadosisContract, WorldwideDayEntryExt},
};

use super::{
    codec::{
        decode_scheduler, encode_scheduler, max_canonical_object_bytes, read_canonical_optional,
    },
    index::ReadyIndexKey,
    live_index::{decode_live_scheduler_index, encode_live_scheduler_index, LiveIndexKey},
    state::{DayPhase, JobFsmState, OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS},
};

impl MetadosisContract<'_> {
    pub(crate) fn request_limit_receipt(
        &self,
        wwd: WorldwideDay,
        limits: &SchemaLimits,
    ) -> Result<Option<RequestLimitSplitReceiptV1>> {
        let bytes = self.ocomp_request_limit_receipts.get_bytes(&wwd);
        read_canonical_optional(
            &bytes,
            max_canonical_object_bytes(limits)?,
            |encoded| RequestLimitSplitReceiptV1::decode_canonical(encoded, limits),
            "OCOMP request limit receipt",
        )
    }

    pub fn ocomp_job_record(
        &self,
        intent_id: B256,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompJobRecordV1>> {
        let storage_key = intent_storage_key(intent_id).map_err(|error| {
            storage_corruption_message(format!("derive OCOMP intent storage key: {error}"))
        })?;
        let bytes = self.ocomp_job_records.get_bytes(&storage_key);
        let record = read_canonical_optional(
            &bytes,
            max_canonical_object_bytes(limits)?,
            |encoded| OcompJobRecordV1::decode_canonical(encoded, limits),
            "OCOMP job record",
        )?;
        if let Some(record) = &record {
            let actual = record.intent.intent_id(limits).map_err(|error| {
                storage_corruption_message(format!("hash stored OCOMP intent: {error}"))
            })?;
            if actual != intent_id {
                return Err(storage_corruption_message(
                    "OCOMP job record key/IntentId mismatch",
                ));
            }
        }
        Ok(record)
    }

    pub fn result_vote_accountability(
        &self,
        job_id: B256,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompVoteAccountabilityV1>> {
        let accountability = read_canonical_optional(
            &self.ocomp_vote_accountability.get_bytes(&job_id),
            max_canonical_object_bytes(limits)?,
            |encoded| OcompVoteAccountabilityV1::decode_canonical(encoded, limits),
            "OCOMP vote accountability",
        )?;
        if accountability
            .as_ref()
            .is_some_and(|accountability| accountability.job_id != job_id)
        {
            return Err(storage_corruption_message(
                "OCOMP vote accountability storage key/JobId mismatch",
            ));
        }
        Ok(accountability)
    }

    pub(crate) fn write_result_vote_accountability(
        &self,
        accountability: &OcompVoteAccountabilityV1,
        limits: &SchemaLimits,
    ) -> Result<()> {
        accountability.validate_semantics(limits).map_err(|error| {
            storage_corruption_message(format!("invalid OCOMP vote accountability: {error}"))
        })?;
        self.ocomp_vote_accountability
            .get_bytes(&accountability.job_id)
            .write(&accountability.encode_canonical(limits).map_err(|error| {
                storage_corruption_message(format!("encode OCOMP vote accountability: {error}"))
            })?)
    }

    pub fn active_lysis_generation(
        &self,
        wwd: WorldwideDay,
        limits: &SchemaLimits,
    ) -> Result<Option<ActiveGenerationV1>> {
        read_canonical_optional(
            &self.ocomp_active_lysis_generations.get_bytes(&wwd),
            max_canonical_object_bytes(limits)?,
            |encoded| ActiveGenerationV1::decode_canonical(encoded, limits),
            "active Lysis generation",
        )
    }

    pub(crate) fn ocomp_fsm_state(
        &self,
        wwd: WorldwideDay,
        schema_limits: &SchemaLimits,
    ) -> Result<JobFsmState> {
        let encoded = self.ocomp_fsm_states.get_bytes(&wwd).read()?;
        if encoded.is_empty() {
            return Err(storage_corruption_message(
                "OCOMP WWD FSM is not initialized",
            ));
        }
        let snapshot = decode_scheduler(&encoded)?;
        if snapshot.worldwide_day != wwd {
            return Err(storage_corruption_message(
                "OCOMP WWD FSM storage key mismatch",
            ));
        }

        let state = JobFsmState::restore(snapshot)
            .map_err(|error| storage_corruption_message(format!("restore OCOMP FSM: {error}")))?;
        self.validate_persisted_equivalences(&state, schema_limits)?;
        Ok(state)
    }

    pub(crate) fn live_ocomp_fsm_state_by_intent(
        &self,
        intent_id: B256,
        schema_limits: &SchemaLimits,
    ) -> Result<Option<JobFsmState>> {
        Ok(self
            .live_ocomp_fsm_states(schema_limits)?
            .into_iter()
            .find(|state| state.projection().live_intent_id == Some(intent_id)))
    }

    pub(crate) fn live_ocomp_fsm_states(
        &self,
        schema_limits: &SchemaLimits,
    ) -> Result<Vec<JobFsmState>> {
        let snapshots = decode_live_scheduler_index(&self.ocomp_scheduler.read()?)?;
        let mut states = Vec::new();
        states
            .try_reserve_exact(snapshots.len())
            .map_err(|_| storage_corruption_message("allocate bounded OCOMP live scheduler"))?;
        for snapshot in snapshots {
            let state = self.ocomp_fsm_state(snapshot.worldwide_day, schema_limits)?;
            if state.projection().phase != DayPhase::OffchainPending
                || state.projection().live_intent_id != Some(snapshot.intent_id)
            {
                return Err(storage_corruption_message("OCOMP live index/FSM mismatch"));
            }
            states.push(state);
        }
        Ok(states)
    }

    fn validate_persisted_equivalences(
        &self,
        state: &JobFsmState,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let projection = state.projection();
        let expected_status = match projection.phase {
            DayPhase::Ready => WwdStatus::Ready.as_u8(),
            DayPhase::OffchainPending => WwdStatus::OffchainPending.as_u8(),
            DayPhase::Terminal => {
                return Err(storage_corruption_message(
                    "terminal OCOMP FSM must not be persisted",
                ))
            }
        };
        if self
            .worldwide_days
            .entry(projection.worldwide_day)
            .status()
            .read()?
            != expected_status
        {
            return Err(storage_corruption_message(
                "OCOMP scheduler/WorldwideDay status mismatch",
            ));
        }

        self.validate_retained_request_effect(state, limits)?;
        match projection.phase {
            DayPhase::Ready => self.validate_ready_equivalence(projection),
            DayPhase::OffchainPending => self.validate_pending_equivalence(projection, limits),
            DayPhase::Terminal => Err(storage_corruption_message(
                "terminal OCOMP FSM must not be persisted",
            )),
        }
    }

    fn validate_retained_request_effect(
        &self,
        state: &JobFsmState,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let projection = state.projection();
        let receipt = self.request_limit_receipt(projection.worldwide_day, limits)?;
        match projection.retained_lysis_limit_minor {
            None if receipt.is_none() && projection.pending_nonce == 0 => {}
            Some(lysis_limit_minor) => {
                let receipt = receipt.ok_or_else(|| {
                    storage_corruption_message("OCOMP retained limit has no receipt")
                })?;
                let expected_hash = receipt.receipt_hash(limits).map_err(|error| {
                    storage_corruption_message(format!("hash stored request receipt: {error}"))
                })?;
                let snapshot = state.snapshot();
                let retained = snapshot
                    .ready
                    .and_then(|ready| ready.retained_effect)
                    .or_else(|| snapshot.live.map(|live| live.retained_effect))
                    .ok_or_else(|| {
                        storage_corruption_message("OCOMP retained effect snapshot is missing")
                    })?;
                let limit_matches = receipt.wwd == projection.worldwide_day.value()
                    && receipt.lysis_limit_minor == lysis_limit_minor;
                let effect_matches = receipt.pending_nonce == retained.effect_nonce
                    && expected_hash == retained.receipt_hash;
                if !limit_matches || !effect_matches {
                    return Err(storage_corruption_message(
                        "OCOMP limit receipt/state mismatch",
                    ));
                }
            }
            _ => {
                return Err(storage_corruption_message(
                    "OCOMP fresh READY state has a residual receipt",
                ))
            }
        }

        Ok(())
    }

    fn validate_ready_equivalence(&self, projection: super::state::JobFsmProjection) -> Result<()> {
        let key = ReadyIndexKey::from_projection(projection)?;
        if self.read_ready_index()?.binary_search(&key).is_err() {
            return Err(storage_corruption_message(
                "OCOMP READY FSM has no exact due-index key",
            ));
        }
        Ok(())
    }

    fn validate_pending_equivalence(
        &self,
        projection: super::state::JobFsmProjection,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let intent_id = projection
            .live_intent_id
            .ok_or_else(|| storage_corruption_message("OCOMP pending FSM has no live intent"))?;
        let record = self.ocomp_job_record(intent_id, limits)?.ok_or_else(|| {
            storage_corruption_message("OCOMP live scheduler key has no job record")
        })?;
        let expected_deadline = Some(expected_live_deadline(&record)?);
        let identity_matches = record.intent.wwd == projection.worldwide_day.value()
            && record.intent.pending_nonce == projection.pending_nonce;
        if record.terminal.is_some()
            || !identity_matches
            || expected_deadline != projection.deadline_height
        {
            return Err(storage_corruption_message(
                "OCOMP live scheduler/job record mismatch",
            ));
        }
        if self
            .read_ready_index()?
            .iter()
            .any(|key| key.worldwide_day == projection.worldwide_day)
        {
            return Err(storage_corruption_message(
                "OCOMP live WWD remains in the READY index",
            ));
        }
        Ok(())
    }

    pub(crate) fn write_ocomp_job_record(
        &self,
        intent_id: B256,
        record: &OcompJobRecordV1,
        limits: &SchemaLimits,
    ) -> Result<()> {
        record.validate_semantics(limits).map_err(|error| {
            storage_corruption_message(format!("invalid OCOMP job record: {error}"))
        })?;
        let encoded = record.encode_canonical(limits).map_err(|error| {
            storage_corruption_message(format!("encode OCOMP job record: {error}"))
        })?;
        let storage_key = intent_storage_key(intent_id).map_err(|error| {
            storage_corruption_message(format!("derive OCOMP intent storage key: {error}"))
        })?;
        self.ocomp_job_records
            .get_bytes(&storage_key)
            .write(&encoded)
    }

    pub(super) fn write_ocomp_state(&self, state: &JobFsmState) -> Result<()> {
        self.ocomp_fsm_states
            .get_bytes(&state.projection().worldwide_day)
            .write(&encode_scheduler(state)?)
    }

    pub(super) fn write_live_scheduler(&self, state: &JobFsmState) -> Result<()> {
        if state.projection().phase != DayPhase::OffchainPending {
            return Err(storage_corruption_message(
                "OCOMP live scheduler requires pending state",
            ));
        }
        let projection = state.projection();
        let intent_id = projection.live_intent_id.ok_or_else(|| {
            storage_corruption_message("OCOMP live scheduler has no live attempt")
        })?;
        let key = LiveIndexKey {
            worldwide_day: projection.worldwide_day,
            intent_id,
        };
        let mut index = decode_live_scheduler_index(&self.ocomp_scheduler.read()?)?;
        if let Some(position) = index.iter().position(|existing| {
            existing.worldwide_day == key.worldwide_day || existing.intent_id == key.intent_id
        }) {
            if index[position] != key {
                return Err(storage_corruption_message(
                    "OCOMP live scheduler identity changed",
                ));
            }
        } else {
            index.push(key);
        }
        index.sort();
        self.ocomp_scheduler
            .write(&encode_live_scheduler_index(&index)?)
    }

    pub(crate) fn remove_live_scheduler(&self, intent_id: B256) -> Result<()> {
        let mut index = decode_live_scheduler_index(&self.ocomp_scheduler.read()?)?;
        let position = index
            .iter()
            .position(|key| key.intent_id == intent_id)
            .ok_or_else(|| {
                storage_corruption_message("OCOMP live scheduler is missing the exact job")
            })?;
        index.remove(position);
        if index.is_empty() {
            self.ocomp_scheduler.clear()
        } else {
            self.ocomp_scheduler
                .write(&encode_live_scheduler_index(&index)?)
        }
    }
}

fn expected_live_deadline(record: &OcompJobRecordV1) -> Result<u64> {
    match record.status {
        OcompJobStatus::AwaitingFinality => Ok(record
            .intent_height
            .checked_add(OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS)
            .ok_or_else(|| {
                storage_corruption_message("OCOMP awaiting-finality deadline overflow")
            })?),
        OcompJobStatus::VotingOpen => Ok(record
            .finalized
            .as_ref()
            .ok_or_else(|| storage_corruption_message("live OCOMP job has no finalized binding"))?
            .deadline_height),
        _ => {
            return Err(storage_corruption_message(
                "terminal OCOMP job remains in the live scheduler",
            ))
        }
    }
}
