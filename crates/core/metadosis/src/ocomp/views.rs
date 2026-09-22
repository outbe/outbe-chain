use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_ocomp_protocol::state::{OcompJobRecordV1, OcompJobStatus};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    error::Result,
    storage::{dsl::missing_record_err, StorageHandle},
};

use crate::{
    aggregate::{ValidatedWwdAggregate, WwdStatus},
    constants::{MAX_ACTIVE_WWDS, MAX_RECORDS_KEPT},
    errors::storage_corruption_message,
    schema::MetadosisContract,
};

use super::{
    codec::{decode_live_scheduler_index, LIVE_INDEX_HEADER_LEN, SCHEDULER_ENCODED_LEN},
    index::{
        READY_INDEX_HEADER_LEN, READY_INDEX_KEY_LEN, RESPONSE_INDEX_HEADER_LEN,
        RESPONSE_INDEX_KEY_LEN,
    },
    schema::poc_schema_limits,
};

/// Returns canonical live jobs in native scheduler order after validating the
/// complete indexed WWD aggregate. This Rust-only view performs no writes.
pub fn read_live_ocomp_jobs(storage: StorageHandle<'_>) -> Result<Vec<(B256, OcompJobRecordV1)>> {
    let limits = poc_schema_limits();
    let contract = MetadosisContract::new(storage.clone());
    preflight_live_job_view(&contract)?;
    let aggregate = ValidatedWwdAggregate::load_and_validate(storage)?;
    let mut pending: BTreeSet<_> = aggregate
        .active_records()
        .filter(|record| record.status == WwdStatus::OffchainPending)
        .map(|record| record.worldwide_day)
        .collect();
    let mut intents = BTreeSet::new();
    let mut jobs = Vec::new();
    for state in contract.live_ocomp_fsm_states(&limits)? {
        let projection = state.projection();
        let intent_id = projection
            .live_intent_id
            .ok_or_else(|| storage_corruption_message("OCOMP live FSM has no live intent"))?;
        if !pending.remove(&projection.worldwide_day) || !intents.insert(intent_id) {
            return Err(storage_corruption_message(
                "OCOMP live jobs do not uniquely match active OFFCHAIN_PENDING days",
            ));
        }
        let record = contract
            .ocomp_job_record(intent_id, &limits)?
            .ok_or_else(|| storage_corruption_message("OCOMP live FSM has no job record"))?;
        if !matches!(
            record.status,
            OcompJobStatus::AwaitingFinality | OcompJobStatus::VotingOpen
        ) || jobs.len() >= MAX_ACTIVE_WWDS
        {
            return Err(storage_corruption_message(
                "OCOMP live jobs contain a terminal record or exceed native capacity",
            ));
        }
        jobs.push((intent_id, record));
    }
    if !pending.is_empty() {
        return Err(storage_corruption_message(
            "OCOMP live jobs omit an active OFFCHAIN_PENDING day",
        ));
    }
    Ok(jobs)
}

/// Bound every variable-sized traversal performed by the native aggregate and
/// scheduler readers before invoking them on snapshot storage.
fn preflight_live_job_view(contract: &MetadosisContract<'_>) -> Result<()> {
    let active_len = usize::try_from(contract.active_wwd.len()?)
        .map_err(|_| storage_corruption_message("Metadosis active WWD length overflow"))?;
    let closed_len = usize::try_from(contract.closed_wwd.len()?)
        .map_err(|_| storage_corruption_message("Metadosis closed WWD length overflow"))?;
    if active_len > MAX_ACTIVE_WWDS || closed_len > MAX_RECORDS_KEPT {
        return Err(storage_corruption_message(
            "Metadosis indexed WWD length exceeds native capacity",
        ));
    }
    for (bytes, header_len, item_len, max_count, label) in [
        (
            &contract.ocomp_scheduler,
            LIVE_INDEX_HEADER_LEN,
            SCHEDULER_ENCODED_LEN,
            usize::from(u16::MAX),
            "OCOMP live scheduler",
        ),
        (
            &contract.ocomp_ready_index,
            READY_INDEX_HEADER_LEN,
            READY_INDEX_KEY_LEN,
            MAX_RECORDS_KEPT,
            "OCOMP READY index",
        ),
        (
            &contract.ocomp_response_deadline_index,
            RESPONSE_INDEX_HEADER_LEN,
            RESPONSE_INDEX_KEY_LEN,
            usize::from(u16::MAX),
            "OCOMP response index",
        ),
    ] {
        let max_bytes = item_len
            .checked_mul(max_count)
            .and_then(|length| length.checked_add(header_len))
            .ok_or_else(|| storage_corruption_message(format!("{label} byte cap overflow")))?;
        if bytes.len()? > max_bytes {
            return Err(storage_corruption_message(format!(
                "{label} exceeds native byte cap"
            )));
        }
    }

    let active: BTreeSet<_> = contract.active_wwd.read_all()?.into_iter().collect();
    let closed = contract.closed_wwd.read_all()?;
    for day in active.iter().chain(closed.iter()) {
        let length = contract.ocomp_fsm_states.get_bytes(day).len()?;
        if length != 0 && length != SCHEDULER_ENCODED_LEN {
            return Err(storage_corruption_message(
                "OCOMP indexed FSM has an invalid native byte length",
            ));
        }
    }
    // The native live reader follows scheduler keys. Reject foreign keys here
    // so they cannot lead it to an FSM whose length was never checked above.
    for snapshot in decode_live_scheduler_index(&contract.ocomp_scheduler.read()?)? {
        if !active.contains(&snapshot.worldwide_day) {
            return Err(storage_corruption_message(
                "OCOMP live scheduler points outside active WWD index",
            ));
        }
    }
    Ok(())
}

/// Returns one complete canonical OCB1 job record through the existing
/// Metadosis public precompile.
pub fn get_offchain_job(storage: StorageHandle<'_>, intent_id: B256) -> Result<Vec<u8>> {
    let limits = poc_schema_limits();
    let record = MetadosisContract::new(storage)
        .ocomp_job_record(intent_id, &limits)?
        .ok_or_else(|| missing_record_err("OcompJobRecordV1"))?;
    record
        .encode_canonical(&limits)
        .map_err(|error| crate::errors::storage_corruption(error.to_string()))
}

/// Returns the complete bounded vote/accountability record selected
/// by consensus for one finalized JobId.
pub fn get_offchain_vote_accountability(
    storage: StorageHandle<'_>,
    job_id: B256,
) -> Result<Vec<u8>> {
    let limits = poc_schema_limits();
    let accountability = MetadosisContract::new(storage)
        .result_vote_accountability(job_id, &limits)?
        .ok_or_else(|| missing_record_err("OcompVoteAccountabilityV1"))?;
    accountability
        .encode_canonical(&limits)
        .map_err(|error| crate::errors::storage_corruption(error.to_string()))
}

/// Returns the canonical active generation selected by completed Metadosis
/// state, never by supervisor-local storage.
pub fn get_active_lysis_generation(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Vec<u8>> {
    let limits = poc_schema_limits();
    let generation = MetadosisContract::new(storage)
        .active_lysis_generation(wwd, &limits)?
        .ok_or_else(|| missing_record_err("ActiveGenerationV1"))?;
    generation
        .encode_canonical(&limits)
        .map_err(|error| crate::errors::storage_corruption(error.to_string()))
}

/// Returns the aggregate receipt embedded in a completed or certified-conflict
/// job record.
pub fn get_lysis_terminal_receipt(storage: StorageHandle<'_>, intent_id: B256) -> Result<Vec<u8>> {
    let limits = poc_schema_limits();
    let record = MetadosisContract::new(storage)
        .ocomp_job_record(intent_id, &limits)?
        .ok_or_else(|| missing_record_err("OcompJobRecordV1"))?;
    let receipt = record
        .terminal
        .and_then(|terminal| terminal.completed_binding)
        .map(|binding| binding.terminal_receipt)
        .ok_or_else(|| missing_record_err("AggregateActivationReceiptV1"))?;
    receipt
        .encode_canonical(&limits)
        .map_err(|error| crate::errors::storage_corruption(error.to_string()))
}
