//! Finalized OCOMP job discovery from the public chain RPC.
//!
//! `OffchainJobRequested` is only an index hint. Authority comes from the
//! canonical request block plus the typed `JobIntent` in local finalized state.

use std::path::PathBuf;

use alloy_primitives::{Bytes, LogData, B256};
use alloy_sol_types::SolEvent as _;
use outbe_metadosis::precompile::IMetadosis;
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    state::{OcompJobRecordV1, OcompJobStatus},
    FinalizedJobSpecV1, FinalizedJobSummaryV1, ProtocolError, SchemaLimits,
};
use thiserror::Error;

use crate::{
    control::EndpointIdentity,
    public_rpc::{FinalizedRpcBlockV1, PublicOcompRpcClientV1, PublicRpcError},
    supervisor::{
        DiscoveryJournal, DiscoveryOutcome, DiscoveryRecord, ScanCheckpointV1,
        SupervisorDiscoveryError,
    },
};

const DISCOVERY_SCAN_CHUNK_BLOCKS: u64 = 100;

#[derive(Clone, Debug)]
pub struct FinalizedRpcDiscoveryConfigV1 {
    pub rpc_url: String,
    pub rpc_max_response_bytes: usize,
    pub journal_root: PathBuf,
    pub projection_start_block: u64,
    pub identity: EndpointIdentity,
    pub fork_id: B256,
    pub limits: SchemaLimits,
    pub purpose: FinalizedRpcDiscoveryPurposeV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizedRpcDiscoveryPurposeV1 {
    /// Authority for validator computation and vote submission. The job must
    /// still be inside its canonical response window.
    VotingAuthority,
    /// Input materialization for node-local calculation/replay. A completed
    /// job remains readable so a FullNode can finish and compare with canon.
    InputReplay,
}

pub struct FinalizedRpcDiscoveryV1 {
    config: FinalizedRpcDiscoveryConfigV1,
    rpc: Box<dyn FinalizedRpcClientV1>,
    journal: DiscoveryJournal,
    journal_validated: bool,
    scan_checkpoint_validated: bool,
}

trait FinalizedRpcClientV1 {
    fn finalized_block(&self) -> Result<FinalizedRpcBlockV1, PublicRpcError>;
    fn block_by_number(&self, number: u64) -> Result<FinalizedRpcBlockV1, PublicRpcError>;
    fn logs(
        &self,
        address: alloy_primitives::Address,
        topic0: B256,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<serde_json::Value>, PublicRpcError>;
    fn job_record_at_hash(
        &self,
        intent_id: B256,
        block_hash: B256,
    ) -> Result<Vec<u8>, PublicRpcError>;
}

impl FinalizedRpcClientV1 for PublicOcompRpcClientV1 {
    fn finalized_block(&self) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
        Self::finalized_block(self)
    }

    fn block_by_number(&self, number: u64) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
        Self::block_by_number(self, number)
    }

    fn logs(
        &self,
        address: alloy_primitives::Address,
        topic0: B256,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<serde_json::Value>, PublicRpcError> {
        Self::logs(self, address, topic0, from_block, to_block)
    }

    fn job_record_at_hash(
        &self,
        intent_id: B256,
        block_hash: B256,
    ) -> Result<Vec<u8>, PublicRpcError> {
        Self::job_record_at_hash(self, intent_id, block_hash)
    }
}

impl FinalizedRpcDiscoveryV1 {
    pub fn open(config: FinalizedRpcDiscoveryConfigV1) -> Result<Self, RpcDiscoveryErrorV1> {
        if config.projection_start_block == 0 || config.fork_id.is_zero() {
            return Err(RpcDiscoveryErrorV1::InvalidConfig);
        }
        let rpc = Box::new(PublicOcompRpcClientV1::new(
            config.rpc_url.clone(),
            config.rpc_max_response_bytes,
        )?);
        Self::open_with_rpc(config, rpc)
    }

    fn open_with_rpc(
        config: FinalizedRpcDiscoveryConfigV1,
        rpc: Box<dyn FinalizedRpcClientV1>,
    ) -> Result<Self, RpcDiscoveryErrorV1> {
        if config.projection_start_block == 0 || config.fork_id.is_zero() {
            return Err(RpcDiscoveryErrorV1::InvalidConfig);
        }
        let journal = DiscoveryJournal::open(&config.journal_root, config.limits)?;
        if journal.scan_checkpoint().is_some_and(|checkpoint| {
            checkpoint.identity != config.identity || checkpoint.fork_id != config.fork_id
        }) {
            return Err(RpcDiscoveryErrorV1::ScanCheckpointIdentityMismatch);
        }
        Ok(Self {
            config,
            rpc,
            journal,
            journal_validated: false,
            scan_checkpoint_validated: false,
        })
    }

    pub fn reconcile_once(&mut self) -> Result<DiscoveryOutcome, RpcDiscoveryErrorV1> {
        self.journal_validated = false;
        let result = self.reconcile_inner();
        if result.is_err() {
            self.journal_validated = false;
        }
        result
    }

    fn reconcile_inner(&mut self) -> Result<DiscoveryOutcome, RpcDiscoveryErrorV1> {
        let finalized_head = self.rpc.finalized_block()?;
        self.validate_scan_checkpoint(finalized_head)?;
        if let Some(record) = self.journal.record() {
            if record.cursor > finalized_head.number {
                return Err(RpcDiscoveryErrorV1::FinalizedHeadBehindJournal {
                    finalized: finalized_head.number,
                    cursor: record.cursor,
                });
            }
            let canonical = self.rpc.block_by_number(record.cursor)?;
            require_journal_anchor(record, canonical)?;
            let current_bytes = self
                .rpc
                .job_record_at_hash(record.spec.summary.intent_id, finalized_head.hash)?;
            let current = OcompJobRecordV1::decode_canonical(&current_bytes, &self.config.limits)?;
            let expected = rebuild_durable_spec(
                &current,
                canonical,
                self.config.identity,
                self.config.fork_id,
                &self.config.limits,
            )?;
            if expected != record.spec {
                return Err(RpcDiscoveryErrorV1::JournalAuthorityMismatch);
            }
            self.journal_validated =
                record_available_for_purpose(self.config.purpose, &current, finalized_head.number);
        }
        let after_cursor = effective_scan_cursor(
            self.config.projection_start_block,
            self.journal.record(),
            self.journal.scan_checkpoint(),
        );
        let Some(from_block) = after_cursor.checked_add(1) else {
            return Err(RpcDiscoveryErrorV1::CursorOverflow);
        };
        if from_block > finalized_head.number {
            return Ok(DiscoveryOutcome::NoNewJob);
        }
        let to_block = bounded_scan_end(from_block, finalized_head.number);
        eprintln!(
            "OCOMP finalized discovery scanning blocks: \
             from_block={from_block} to_block={to_block} finalized_head={}",
            finalized_head.number
        );

        let logs = self.rpc.logs(
            outbe_primitives::addresses::METADOSIS_ADDRESS,
            IMetadosis::OffchainJobRequested::SIGNATURE_HASH,
            from_block,
            to_block,
        )?;
        let mut previous_position = None;
        for value in logs {
            let indexed = decode_request_log(&value)?;
            let position = (indexed.block_number, indexed.log_index);
            if previous_position.is_some_and(|previous| previous >= position) {
                return Err(RpcDiscoveryErrorV1::NonCanonicalLogOrder);
            }
            previous_position = Some(position);
            if indexed.block_number < from_block || indexed.block_number > to_block {
                return Err(RpcDiscoveryErrorV1::EventOutsideRequestedRange);
            }
            let canonical = self.rpc.block_by_number(indexed.block_number)?;
            if canonical.hash != indexed.block_hash {
                return Err(RpcDiscoveryErrorV1::EventBlockHash);
            }

            let current_bytes = self
                .rpc
                .job_record_at_hash(indexed.intent_id, finalized_head.hash)?;
            let current = OcompJobRecordV1::decode_canonical(&current_bytes, &self.config.limits)?;
            match record_availability_for_purpose(
                self.config.purpose,
                &current,
                finalized_head.number,
            ) {
                RecordAvailabilityV1::Pending => {
                    self.persist_scan_checkpoint_before(indexed.block_number)?;
                    return Ok(DiscoveryOutcome::NoNewJob);
                }
                RecordAvailabilityV1::Terminal => continue,
                RecordAvailabilityV1::Available => {}
            }

            let spec = build_finalized_job_spec(
                &indexed,
                &current,
                canonical,
                self.config.identity,
                self.config.fork_id,
                &self.config.limits,
            )?;
            let record = self.journal.persist(spec)?;
            self.journal_validated = true;
            return Ok(DiscoveryOutcome::Discovered(Box::new(record)));
        }
        let anchor = if to_block == finalized_head.number {
            finalized_head
        } else {
            self.rpc.block_by_number(to_block)?
        };
        self.persist_scan_checkpoint(to_block, anchor)?;
        Ok(DiscoveryOutcome::NoNewJob)
    }

    fn validate_scan_checkpoint(
        &mut self,
        finalized_head: FinalizedRpcBlockV1,
    ) -> Result<(), RpcDiscoveryErrorV1> {
        if self.scan_checkpoint_validated {
            return Ok(());
        }
        if let Some(checkpoint) = self.journal.scan_checkpoint().copied() {
            if checkpoint.scanned_through_height > finalized_head.number {
                return Err(RpcDiscoveryErrorV1::FinalizedHeadBehindScanCheckpoint {
                    finalized: finalized_head.number,
                    checkpoint: checkpoint.scanned_through_height,
                });
            }
            let canonical = self
                .rpc
                .block_by_number(checkpoint.scanned_through_height)?;
            require_scan_checkpoint_anchor(checkpoint, canonical)?;
        }
        self.scan_checkpoint_validated = true;
        Ok(())
    }

    fn persist_scan_checkpoint_before(
        &mut self,
        blocking_height: u64,
    ) -> Result<(), RpcDiscoveryErrorV1> {
        let Some(safe_height) = blocking_height.checked_sub(1) else {
            return Err(RpcDiscoveryErrorV1::CursorOverflow);
        };
        let current = effective_scan_cursor(
            self.config.projection_start_block,
            self.journal.record(),
            self.journal.scan_checkpoint(),
        );
        if safe_height <= current {
            return Ok(());
        }
        let anchor = self.rpc.block_by_number(safe_height)?;
        self.persist_scan_checkpoint(safe_height, anchor)
    }

    fn persist_scan_checkpoint(
        &mut self,
        height: u64,
        canonical: FinalizedRpcBlockV1,
    ) -> Result<(), RpcDiscoveryErrorV1> {
        if canonical.number != height || canonical.hash.is_zero() {
            return Err(RpcDiscoveryErrorV1::ScanCheckpointAuthorityMismatch);
        }
        let previous_height = self
            .journal
            .scan_checkpoint()
            .map(|checkpoint| checkpoint.scanned_through_height);
        let checkpoint = self.journal.persist_scan_checkpoint(
            self.config.identity,
            self.config.fork_id,
            height,
            canonical.hash,
        )?;
        if previous_height.is_none_or(|previous| checkpoint.scanned_through_height > previous) {
            eprintln!(
                "OCOMP finalized discovery advanced scan checkpoint: \
                 scanned_through_height={} scanned_through_block_hash={:#x}",
                checkpoint.scanned_through_height, checkpoint.scanned_through_block_hash
            );
        }
        self.scan_checkpoint_validated = true;
        Ok(())
    }

    pub fn current_record(&self) -> Option<DiscoveryRecord> {
        self.journal_validated
            .then(|| self.journal.record().cloned())
            .flatten()
    }
}

fn bounded_scan_end(from_block: u64, finalized_height: u64) -> u64 {
    from_block
        .saturating_add(DISCOVERY_SCAN_CHUNK_BLOCKS - 1)
        .min(finalized_height)
}

fn effective_scan_cursor(
    projection_start_block: u64,
    record: Option<&DiscoveryRecord>,
    checkpoint: Option<&ScanCheckpointV1>,
) -> u64 {
    let initial = projection_start_block.saturating_sub(1);
    let job_cursor = record.map_or(initial, |record| record.cursor);
    checkpoint.map_or(job_cursor, |checkpoint| {
        job_cursor.max(checkpoint.scanned_through_height)
    })
}

fn rebuild_durable_spec(
    current: &OcompJobRecordV1,
    request: FinalizedRpcBlockV1,
    identity: EndpointIdentity,
    fork_id: B256,
    limits: &SchemaLimits,
) -> Result<FinalizedJobSpecV1, RpcDiscoveryErrorV1> {
    let intent = &current.intent;
    let indexed = IndexedJobRequestV1 {
        intent_id: intent.intent_id(limits)?,
        worldwide_day: intent.wwd,
        pending_nonce: intent.pending_nonce,
        attempt: intent.attempt,
        activation_preconditions_hash: intent
            .activation_preconditions
            .activation_preconditions_hash(limits)?,
        block_number: request.number,
        block_hash: request.hash,
        // Log position is locator metadata and is deliberately not part of the
        // durable authority rebuilt from finalized state.
        log_index: 0,
    };
    build_finalized_job_spec(&indexed, current, request, identity, fork_id, limits)
}

fn record_available_for_purpose(
    purpose: FinalizedRpcDiscoveryPurposeV1,
    record: &OcompJobRecordV1,
    finalized_height: u64,
) -> bool {
    record_availability_for_purpose(purpose, record, finalized_height)
        == RecordAvailabilityV1::Available
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordAvailabilityV1 {
    Pending,
    Available,
    Terminal,
}

fn record_availability_for_purpose(
    purpose: FinalizedRpcDiscoveryPurposeV1,
    record: &OcompJobRecordV1,
    finalized_height: u64,
) -> RecordAvailabilityV1 {
    let Some(finalized) = record.finalized.as_ref() else {
        return RecordAvailabilityV1::Pending;
    };
    match purpose {
        FinalizedRpcDiscoveryPurposeV1::VotingAuthority => match record.status {
            OcompJobStatus::AwaitingFinality => RecordAvailabilityV1::Pending,
            OcompJobStatus::VotingOpen if finalized_height < finalized.open_height => {
                RecordAvailabilityV1::Pending
            }
            OcompJobStatus::VotingOpen if finalized_height < finalized.deadline_height => {
                RecordAvailabilityV1::Available
            }
            OcompJobStatus::VotingOpen
            | OcompJobStatus::Completed
            | OcompJobStatus::Expired
            | OcompJobStatus::Conflicted
            | OcompJobStatus::Canceled => RecordAvailabilityV1::Terminal,
        },
        FinalizedRpcDiscoveryPurposeV1::InputReplay => match record.status {
            OcompJobStatus::AwaitingFinality => RecordAvailabilityV1::Pending,
            OcompJobStatus::VotingOpen | OcompJobStatus::Completed => {
                RecordAvailabilityV1::Available
            }
            OcompJobStatus::Expired | OcompJobStatus::Conflicted | OcompJobStatus::Canceled => {
                RecordAvailabilityV1::Terminal
            }
        },
    }
}

fn require_journal_anchor(
    record: &DiscoveryRecord,
    canonical: FinalizedRpcBlockV1,
) -> Result<(), RpcDiscoveryErrorV1> {
    if canonical.number != record.cursor
        || canonical.hash != record.spec.summary.finalized_block_hash
        || canonical.state_root != record.spec.summary.finalized_state_root
    {
        return Err(RpcDiscoveryErrorV1::JournalAuthorityMismatch);
    }
    Ok(())
}

fn require_scan_checkpoint_anchor(
    checkpoint: ScanCheckpointV1,
    canonical: FinalizedRpcBlockV1,
) -> Result<(), RpcDiscoveryErrorV1> {
    if canonical.number != checkpoint.scanned_through_height
        || canonical.hash != checkpoint.scanned_through_block_hash
    {
        return Err(RpcDiscoveryErrorV1::ScanCheckpointAuthorityMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndexedJobRequestV1 {
    intent_id: B256,
    worldwide_day: u32,
    pending_nonce: u64,
    attempt: u32,
    activation_preconditions_hash: B256,
    block_number: u64,
    block_hash: B256,
    log_index: u64,
}

fn decode_request_log(
    value: &serde_json::Value,
) -> Result<IndexedJobRequestV1, RpcDiscoveryErrorV1> {
    if value.get("removed").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(RpcDiscoveryErrorV1::RemovedLog);
    }
    let topics = value
        .get("topics")
        .and_then(serde_json::Value::as_array)
        .ok_or(RpcDiscoveryErrorV1::MalformedLog)?
        .iter()
        .map(|topic| {
            topic
                .as_str()
                .ok_or(RpcDiscoveryErrorV1::MalformedLog)?
                .parse::<B256>()
                .map_err(|_| RpcDiscoveryErrorV1::MalformedLog)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or(RpcDiscoveryErrorV1::MalformedLog)?;
    let data = hex::decode(data.strip_prefix("0x").unwrap_or(data))
        .map_err(|_| RpcDiscoveryErrorV1::MalformedLog)?;
    let event = IMetadosis::OffchainJobRequested::decode_log_data(&LogData::new_unchecked(
        topics,
        Bytes::from(data),
    ))
    .map_err(|_| RpcDiscoveryErrorV1::MalformedLog)?;
    Ok(IndexedJobRequestV1 {
        intent_id: event.intentId,
        worldwide_day: event.wwd,
        pending_nonce: event.pendingNonce,
        attempt: event.attempt,
        activation_preconditions_hash: event.activationPreconditionsHash,
        block_number: parse_rpc_u64(value, "blockNumber")?,
        block_hash: parse_rpc_b256(value, "blockHash")?,
        log_index: parse_rpc_u64(value, "logIndex")?,
    })
}

fn build_finalized_job_spec(
    indexed: &IndexedJobRequestV1,
    current: &OcompJobRecordV1,
    request: FinalizedRpcBlockV1,
    identity: EndpointIdentity,
    fork_id: B256,
    limits: &SchemaLimits,
) -> Result<FinalizedJobSpecV1, RpcDiscoveryErrorV1> {
    let finalized = current
        .finalized
        .as_ref()
        .ok_or(RpcDiscoveryErrorV1::EventAuthorityMismatch)?;
    let intent = &current.intent;
    let intent_id = intent.intent_id(limits)?;
    let job_id = intent.job_id(request.hash, request.state_root, limits)?;
    if current.intent_height != indexed.block_number
        || request.number != indexed.block_number
        || request.hash != indexed.block_hash
        || finalized.finalized_request_block_hash != request.hash
        || finalized.finalized_request_state_root != request.state_root
        || finalized.job_id != job_id
        || intent_id != indexed.intent_id
        || intent.chain_id != identity.chain_id
        || intent.genesis_hash != identity.genesis_hash
        || intent.fork_id != fork_id
        || intent.protocol_bundle_hash != identity.protocol_bundle_hash
        || intent.wwd != indexed.worldwide_day
        || intent.pending_nonce != indexed.pending_nonce
        || intent.attempt != indexed.attempt
        || intent
            .activation_preconditions
            .activation_preconditions_hash(limits)?
            != indexed.activation_preconditions_hash
    {
        return Err(RpcDiscoveryErrorV1::EventAuthorityMismatch);
    }
    Ok(FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: indexed.block_number,
            job_id,
            intent_id,
            finalized_block_hash: request.hash,
            finalized_state_root: request.state_root,
            protocol_bundle_hash: intent.protocol_bundle_hash,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
        },
        canonical_job_intent: BoundedBytes(intent.encode_canonical(limits)?),
    })
}

fn parse_rpc_u64(value: &serde_json::Value, field: &str) -> Result<u64, RpcDiscoveryErrorV1> {
    let encoded = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(RpcDiscoveryErrorV1::MalformedLog)?;
    u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16)
        .map_err(|_| RpcDiscoveryErrorV1::MalformedLog)
}

fn parse_rpc_b256(value: &serde_json::Value, field: &str) -> Result<B256, RpcDiscoveryErrorV1> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(RpcDiscoveryErrorV1::MalformedLog)?
        .parse()
        .map_err(|_| RpcDiscoveryErrorV1::MalformedLog)
}

#[derive(Debug, Error)]
pub enum RpcDiscoveryErrorV1 {
    #[error(transparent)]
    Rpc(#[from] PublicRpcError),
    #[error(transparent)]
    Journal(#[from] SupervisorDiscoveryError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("finalized RPC discovery configuration is invalid")]
    InvalidConfig,
    #[error("finalized RPC discovery cursor overflow")]
    CursorOverflow,
    #[error("finalized OCOMP request logs are not in canonical order")]
    NonCanonicalLogOrder,
    #[error("finalized OCOMP request event did not advance the durable cursor")]
    NonMonotonicEventCursor,
    #[error("finalized OCOMP request event is outside the requested block range")]
    EventOutsideRequestedRange,
    #[error("finalized OCOMP request log is marked removed")]
    RemovedLog,
    #[error("finalized OCOMP request log is malformed")]
    MalformedLog,
    #[error("finalized OCOMP request log block hash differs from the canonical block")]
    EventBlockHash,
    #[error("finalized OCOMP request event does not match the authenticated job")]
    EventAuthorityMismatch,
    #[error("finalized RPC head {finalized} is behind durable OCOMP discovery cursor {cursor}")]
    FinalizedHeadBehindJournal { finalized: u64, cursor: u64 },
    #[error("finalized RPC head {finalized} is behind durable OCOMP scan checkpoint {checkpoint}")]
    FinalizedHeadBehindScanCheckpoint { finalized: u64, checkpoint: u64 },
    #[error("durable OCOMP discovery record differs from the canonical finalized block")]
    JournalAuthorityMismatch,
    #[error("durable OCOMP scan checkpoint identity differs from the active endpoint")]
    ScanCheckpointIdentityMismatch,
    #[error("durable OCOMP scan checkpoint differs from its canonical finalized block")]
    ScanCheckpointAuthorityMismatch,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use alloy_primitives::U256;
    use outbe_ocomp_protocol::{
        intent::{
            ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
            FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
            MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
        },
        profile::poc_schema_limits,
        state::OcompFinalizedJobV1,
    };

    use super::*;

    #[derive(Default)]
    struct MockRpcState {
        finalized_height: u64,
        log_ranges: Vec<(u64, u64)>,
        fail_logs: bool,
        logs: Vec<serde_json::Value>,
        job_record: Option<Vec<u8>>,
        block_override: Option<FinalizedRpcBlockV1>,
    }

    #[derive(Clone)]
    struct MockRpc {
        state: Arc<Mutex<MockRpcState>>,
    }

    impl MockRpc {
        fn new(finalized_height: u64) -> (Self, Arc<Mutex<MockRpcState>>) {
            let state = Arc::new(Mutex::new(MockRpcState {
                finalized_height,
                ..Default::default()
            }));
            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    impl FinalizedRpcClientV1 for MockRpc {
        fn finalized_block(&self) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
            let height = self.state.lock().unwrap().finalized_height;
            Ok(mock_block(height, None))
        }

        fn block_by_number(&self, number: u64) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
            let block_override = self.state.lock().unwrap().block_override;
            Ok(block_override
                .filter(|block| block.number == number)
                .unwrap_or_else(|| mock_block(number, None)))
        }

        fn logs(
            &self,
            _address: alloy_primitives::Address,
            _topic0: B256,
            from_block: u64,
            to_block: u64,
        ) -> Result<Vec<serde_json::Value>, PublicRpcError> {
            let mut state = self.state.lock().unwrap();
            state.log_ranges.push((from_block, to_block));
            if state.fail_logs {
                return Err(PublicRpcError::Remote {
                    method: "eth_getLogs",
                    detail: "injected failure".to_owned(),
                });
            }
            Ok(state
                .logs
                .iter()
                .filter(|value| {
                    parse_rpc_u64(value, "blockNumber")
                        .is_ok_and(|height| height >= from_block && height <= to_block)
                })
                .cloned()
                .collect())
        }

        fn job_record_at_hash(
            &self,
            _intent_id: B256,
            _block_hash: B256,
        ) -> Result<Vec<u8>, PublicRpcError> {
            self.state
                .lock()
                .unwrap()
                .job_record
                .clone()
                .ok_or(PublicRpcError::Remote {
                    method: "eth_call",
                    detail: "no mock job record".to_owned(),
                })
        }
    }

    fn mock_block(number: u64, hash: Option<B256>) -> FinalizedRpcBlockV1 {
        FinalizedRpcBlockV1 {
            number,
            hash: hash.unwrap_or_else(|| numbered_hash(0x51, number)),
            state_root: numbered_hash(0x52, number),
        }
    }

    fn numbered_hash(domain: u8, number: u64) -> B256 {
        let mut bytes = [0_u8; 32];
        bytes[0] = domain;
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        B256::from(bytes)
    }

    fn discovery_config(
        journal_root: PathBuf,
        identity: EndpointIdentity,
        fork_id: B256,
        purpose: FinalizedRpcDiscoveryPurposeV1,
    ) -> FinalizedRpcDiscoveryConfigV1 {
        FinalizedRpcDiscoveryConfigV1 {
            rpc_url: "mock://finalized".to_owned(),
            rpc_max_response_bytes: 1024,
            journal_root,
            projection_start_block: 1,
            identity,
            fork_id,
            limits: poc_schema_limits(),
            purpose,
        }
    }

    fn request_log(indexed: IndexedJobRequestV1) -> serde_json::Value {
        let event = IMetadosis::OffchainJobRequested {
            intentId: indexed.intent_id,
            wwd: indexed.worldwide_day,
            pendingNonce: indexed.pending_nonce,
            attempt: indexed.attempt,
            activationPreconditionsHash: indexed.activation_preconditions_hash,
        };
        let encoded = event.encode_log_data();
        serde_json::json!({
            "removed": false,
            "blockNumber": format!("0x{:x}", indexed.block_number),
            "blockHash": format!("{:#x}", indexed.block_hash),
            "logIndex": format!("0x{:x}", indexed.log_index),
            "topics": encoded.topics().iter().map(|topic| format!("{topic:#x}")).collect::<Vec<_>>(),
            "data": format!("0x{}", hex::encode(encoded.data.as_ref())),
        })
    }

    fn finalized_record_fixture() -> (
        EndpointIdentity,
        B256,
        IndexedJobRequestV1,
        FinalizedRpcBlockV1,
        OcompJobRecordV1,
    ) {
        let limits = poc_schema_limits();
        let identity = EndpointIdentity {
            chain_id: 42,
            genesis_hash: B256::repeat_byte(0x40),
            boot_nonce: B256::repeat_byte(0x41),
            protocol_bundle_hash: B256::repeat_byte(0x42),
        };
        let fork_id = B256::repeat_byte(0x43);
        let collection_key = B256::repeat_byte(0x44);
        let collection_root = B256::repeat_byte(0x45);
        let intent = JobIntentV1 {
            chain_id: identity.chain_id,
            genesis_hash: identity.genesis_hash,
            fork_id,
            wwd: 20_260_809,
            pending_nonce: 1,
            attempt: 1,
            protocol_bundle_hash: identity.protocol_bundle_hash,
            ce_sealed_root: B256::repeat_byte(0x46),
            sealed_tribute_collection_key: collection_key,
            sealed_tribute_collection_root: collection_root,
            authenticated_day_count: 1,
            authenticated_day_nominal: U256::from(10),
            pre_admission_envelope_hash: B256::repeat_byte(0x47),
            source_availability_policy_id: B256::repeat_byte(0x48),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: U256::from(100),
                previous_vwap: U256::from(90),
                current_vwap: U256::from(100),
                gratis_demand: U256::ZERO,
                gratis_supply: U256::ZERO,
                lysis_budget: U256::from(80),
                auction_base: U256::from(20),
                auction_entry_price: U256::from(95),
                request_budget_split_receipt_hash: B256::repeat_byte(0x49),
            },
            logical_evaluation_height: 90,
            logical_evaluation_time: 1_000,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: 20_260_809,
                    source_generation: 1,
                    collection_key,
                    sealed_collection_root: collection_root,
                    exact_count: 1,
                    exact_nominal_total: U256::from(10),
                },
                nod: NodTargetPreconditionV1 {
                    wwd: 20_260_809,
                    target_generation: 1,
                    namespace_root_before: B256::repeat_byte(0x4a),
                    max_nod_count: 1,
                },
                contributors: ContributorTargetPreconditionV1 {
                    series_id: 20_260_809,
                    expected_series_version: 1,
                    max_contributor_count: 1,
                    max_eligible_nominal_total: U256::from(10),
                },
                metadosis: MetadosisAttemptPreconditionV1 {
                    wwd: 20_260_809,
                    pending_nonce: 1,
                    expected_status: MetadosisExpectedStatus::OffchainPending,
                    state_version: 1,
                },
            },
            result_validator_set_epoch: 3,
            result_committee_set_hash: B256::repeat_byte(0x4b),
            result_ocomp_binding_hash: B256::repeat_byte(0x4c),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        };
        let request = FinalizedRpcBlockV1 {
            number: 100,
            hash: B256::repeat_byte(0x50),
            state_root: B256::repeat_byte(0x51),
        };
        let intent_id = intent.intent_id(&limits).unwrap();
        let activation_preconditions_hash = intent
            .activation_preconditions
            .activation_preconditions_hash(&limits)
            .unwrap();
        let job_id = intent
            .job_id(request.hash, request.state_root, &limits)
            .unwrap();
        let indexed = IndexedJobRequestV1 {
            intent_id,
            worldwide_day: intent.wwd,
            pending_nonce: intent.pending_nonce,
            attempt: intent.attempt,
            activation_preconditions_hash,
            block_number: request.number,
            block_hash: request.hash,
            log_index: 0,
        };
        let record = OcompJobRecordV1 {
            intent,
            intent_height: request.number,
            status: OcompJobStatus::VotingOpen,
            finalized: Some(OcompFinalizedJobV1 {
                job_id,
                finalized_request_block_hash: request.hash,
                finalized_request_state_root: request.state_root,
                finality_recorded_height: 101,
                open_height: 105,
                deadline_height: 120,
                quorum: None,
            }),
            terminal: None,
        };
        (identity, fork_id, indexed, request, record)
    }

    #[test]
    fn empty_finalized_scans_advance_in_restart_safe_hundred_block_chunks() {
        let (identity, fork_id, _, _, _) = finalized_record_fixture();
        let root = tempfile::tempdir().unwrap();
        let (first_rpc, first_state) = MockRpc::new(250);
        let mut first = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(first_rpc),
        )
        .unwrap();

        assert_eq!(first.reconcile_once().unwrap(), DiscoveryOutcome::NoNewJob);
        assert_eq!(first_state.lock().unwrap().log_ranges, vec![(1, 100)]);
        assert_eq!(
            first
                .journal
                .scan_checkpoint()
                .unwrap()
                .scanned_through_height,
            100
        );
        drop(first);

        let (restart_rpc, restart_state) = MockRpc::new(250);
        let mut restarted = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(restart_rpc),
        )
        .unwrap();
        assert_eq!(
            restarted.reconcile_once().unwrap(),
            DiscoveryOutcome::NoNewJob
        );
        assert_eq!(restart_state.lock().unwrap().log_ranges, vec![(101, 200)]);
        assert_eq!(
            restarted
                .journal
                .scan_checkpoint()
                .unwrap()
                .scanned_through_height,
            200
        );

        assert_eq!(
            restarted.reconcile_once().unwrap(),
            DiscoveryOutcome::NoNewJob
        );
        assert_eq!(
            restart_state.lock().unwrap().log_ranges,
            vec![(101, 200), (201, 250)]
        );
        assert_eq!(
            restarted
                .journal
                .scan_checkpoint()
                .unwrap()
                .scanned_through_height,
            250
        );
    }

    #[test]
    fn failed_empty_scan_does_not_advance_checkpoint() {
        let (identity, fork_id, _, _, _) = finalized_record_fixture();
        let root = tempfile::tempdir().unwrap();
        let (rpc, state) = MockRpc::new(100);
        state.lock().unwrap().fail_logs = true;
        let mut discovery = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(rpc),
        )
        .unwrap();

        assert!(matches!(
            discovery.reconcile_once(),
            Err(RpcDiscoveryErrorV1::Rpc(PublicRpcError::Remote { .. }))
        ));
        assert!(discovery.journal.scan_checkpoint().is_none());
        state.lock().unwrap().fail_logs = false;
        assert_eq!(
            discovery.reconcile_once().unwrap(),
            DiscoveryOutcome::NoNewJob
        );
        assert_eq!(state.lock().unwrap().log_ranges, vec![(1, 100), (1, 100)]);
    }

    #[test]
    fn restart_rejects_a_scan_checkpoint_whose_finalized_hash_changed() {
        let (identity, fork_id, _, _, _) = finalized_record_fixture();
        let root = tempfile::tempdir().unwrap();
        let (rpc, _) = MockRpc::new(100);
        let mut discovery = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(rpc),
        )
        .unwrap();
        discovery.reconcile_once().unwrap();
        drop(discovery);

        let (restart_rpc, restart_state) = MockRpc::new(101);
        restart_state.lock().unwrap().block_override = Some(FinalizedRpcBlockV1 {
            number: 100,
            hash: B256::repeat_byte(0xee),
            state_root: numbered_hash(0x52, 100),
        });
        let mut restarted = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(restart_rpc),
        )
        .unwrap();
        assert!(matches!(
            restarted.reconcile_once(),
            Err(RpcDiscoveryErrorV1::ScanCheckpointAuthorityMismatch)
        ));
        assert!(restart_state.lock().unwrap().log_ranges.is_empty());
    }

    #[test]
    fn pending_locator_stops_scan_before_event_and_is_discovered_when_available() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();
        let root = tempfile::tempdir().unwrap();
        let (rpc, state) = MockRpc::new(110);
        let mut pending = record.clone();
        pending.status = OcompJobStatus::AwaitingFinality;
        pending.finalized = None;
        {
            let mut state = state.lock().unwrap();
            state.logs = vec![request_log(indexed)];
            state.job_record = Some(pending.encode_canonical(&limits).unwrap());
            state.block_override = Some(request);
        }
        let mut discovery = FinalizedRpcDiscoveryV1::open_with_rpc(
            discovery_config(
                root.path().to_path_buf(),
                identity,
                fork_id,
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
            ),
            Box::new(rpc),
        )
        .unwrap();

        assert_eq!(
            discovery.reconcile_once().unwrap(),
            DiscoveryOutcome::NoNewJob
        );
        assert_eq!(
            discovery
                .journal
                .scan_checkpoint()
                .unwrap()
                .scanned_through_height,
            request.number - 1
        );

        state.lock().unwrap().job_record = Some(record.encode_canonical(&limits).unwrap());
        assert!(matches!(
            discovery.reconcile_once().unwrap(),
            DiscoveryOutcome::Discovered(_)
        ));
        assert_eq!(discovery.journal.record().unwrap().cursor, request.number);
        assert_eq!(state.lock().unwrap().log_ranges, vec![(1, 100), (100, 110)]);
    }

    #[test]
    fn decodes_the_exact_finalized_request_event_position_and_payload() {
        let event = IMetadosis::OffchainJobRequested {
            intentId: B256::repeat_byte(0x11),
            wwd: 20_260_805,
            pendingNonce: 7,
            attempt: 3,
            activationPreconditionsHash: B256::repeat_byte(0x22),
        };
        let encoded = event.encode_log_data();
        let value = serde_json::json!({
            "removed": false,
            "blockNumber": "0x2a",
            "blockHash": format!("{:#x}", B256::repeat_byte(0x33)),
            "logIndex": "0x5",
            "topics": encoded.topics().iter().map(|topic| format!("{topic:#x}")).collect::<Vec<_>>(),
            "data": format!("0x{}", hex::encode(encoded.data.as_ref())),
        });

        let decoded = decode_request_log(&value).unwrap();
        assert_eq!(decoded.intent_id, event.intentId);
        assert_eq!(decoded.worldwide_day, event.wwd);
        assert_eq!(decoded.pending_nonce, event.pendingNonce);
        assert_eq!(decoded.attempt, event.attempt);
        assert_eq!(
            decoded.activation_preconditions_hash,
            event.activationPreconditionsHash
        );
        assert_eq!((decoded.block_number, decoded.log_index), (42, 5));
    }

    #[test]
    fn rejects_removed_request_events() {
        assert!(matches!(
            decode_request_log(&serde_json::json!({ "removed": true })),
            Err(RpcDiscoveryErrorV1::RemovedLog)
        ));
    }

    #[test]
    fn finalized_state_record_builds_job_spec_without_external_proof_reconstruction() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();

        let spec = build_finalized_job_spec(&indexed, &record, request, identity, fork_id, &limits)
            .unwrap();

        assert_eq!(spec.summary.intent_id, indexed.intent_id);
        assert_eq!(spec.summary.job_id, record.finalized.unwrap().job_id);
        assert_eq!(spec.summary.finalized_block_hash, request.hash);
        assert_eq!(spec.summary.finalized_state_root, request.state_root);
        assert_eq!(
            JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap(),
            record.intent
        );
    }

    #[test]
    fn finalized_state_record_rejects_request_root_or_chain_identity_substitution() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();

        let mut wrong_request = request;
        wrong_request.state_root = B256::repeat_byte(0xee);
        assert!(matches!(
            build_finalized_job_spec(&indexed, &record, wrong_request, identity, fork_id, &limits,),
            Err(RpcDiscoveryErrorV1::EventAuthorityMismatch)
        ));

        let mut wrong_identity = identity;
        wrong_identity.genesis_hash = B256::repeat_byte(0xef);
        assert!(matches!(
            build_finalized_job_spec(&indexed, &record, request, wrong_identity, fork_id, &limits,),
            Err(RpcDiscoveryErrorV1::EventAuthorityMismatch)
        ));
    }

    #[test]
    fn finalized_state_record_rejects_job_and_event_substitutions() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();
        let rejects = |indexed: &IndexedJobRequestV1, record: &OcompJobRecordV1| {
            assert!(matches!(
                build_finalized_job_spec(indexed, record, request, identity, fork_id, &limits,),
                Err(RpcDiscoveryErrorV1::EventAuthorityMismatch)
            ));
        };

        let mut wrong_height = record.clone();
        wrong_height.intent_height = request.number - 1;
        rejects(&indexed, &wrong_height);

        let mut wrong_job = record.clone();
        wrong_job.finalized.as_mut().unwrap().job_id = B256::repeat_byte(0xe1);
        rejects(&indexed, &wrong_job);

        let mut wrong_event = indexed;
        wrong_event.pending_nonce += 1;
        rejects(&wrong_event, &record);
    }

    #[test]
    fn discovery_purpose_separates_vote_deadline_from_full_node_replay() {
        let (_, _, _, _, mut record) = finalized_record_fixture();

        assert!(!record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            &record,
            104,
        ));
        assert!(record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            &record,
            105,
        ));
        assert!(record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            &record,
            119,
        ));
        assert!(!record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            &record,
            120,
        ));

        record.status = OcompJobStatus::Completed;
        assert!(!record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            &record,
            120,
        ));
        assert!(record_available_for_purpose(
            FinalizedRpcDiscoveryPurposeV1::InputReplay,
            &record,
            120,
        ));

        for unavailable in [
            OcompJobStatus::AwaitingFinality,
            OcompJobStatus::Expired,
            OcompJobStatus::Conflicted,
            OcompJobStatus::Canceled,
        ] {
            record.status = unavailable;
            assert!(!record_available_for_purpose(
                FinalizedRpcDiscoveryPurposeV1::InputReplay,
                &record,
                120,
            ));
        }
    }

    #[test]
    fn durable_discovery_record_requires_the_same_finalized_hash_and_root_after_restart() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();
        let spec = build_finalized_job_spec(&indexed, &record, request, identity, fork_id, &limits)
            .unwrap();
        let durable = DiscoveryRecord {
            generation: 1,
            cursor: request.number,
            spec,
        };

        require_journal_anchor(&durable, request).unwrap();

        let mut different_hash = request;
        different_hash.hash = B256::repeat_byte(0xdd);
        assert!(matches!(
            require_journal_anchor(&durable, different_hash),
            Err(RpcDiscoveryErrorV1::JournalAuthorityMismatch)
        ));

        let mut different_root = request;
        different_root.state_root = B256::repeat_byte(0xde);
        assert!(matches!(
            require_journal_anchor(&durable, different_root),
            Err(RpcDiscoveryErrorV1::JournalAuthorityMismatch)
        ));
    }

    #[test]
    fn durable_discovery_spec_is_rebuilt_from_exact_head_state_after_restart() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, mut record) = finalized_record_fixture();
        let durable =
            build_finalized_job_spec(&indexed, &record, request, identity, fork_id, &limits)
                .unwrap();

        assert_eq!(
            rebuild_durable_spec(&record, request, identity, fork_id, &limits).unwrap(),
            durable
        );

        record.finalized.as_mut().unwrap().open_height += 1;
        assert_ne!(
            rebuild_durable_spec(&record, request, identity, fork_id, &limits).unwrap(),
            durable
        );
    }

    #[test]
    fn failed_restart_reconciliation_hides_the_unvalidated_durable_record() {
        let limits = poc_schema_limits();
        let (identity, fork_id, indexed, request, record) = finalized_record_fixture();
        let spec = build_finalized_job_spec(&indexed, &record, request, identity, fork_id, &limits)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut journal = DiscoveryJournal::open(root.path(), limits).unwrap();
        journal.persist(spec).unwrap();
        let mut discovery = FinalizedRpcDiscoveryV1 {
            config: FinalizedRpcDiscoveryConfigV1 {
                rpc_url: "http://127.0.0.1:1".to_owned(),
                rpc_max_response_bytes: 1024,
                journal_root: root.path().to_path_buf(),
                projection_start_block: 1,
                identity,
                fork_id,
                limits,
                purpose: FinalizedRpcDiscoveryPurposeV1::VotingAuthority,
            },
            rpc: Box::new(PublicOcompRpcClientV1::new("http://127.0.0.1:1", 1024).unwrap()),
            journal,
            journal_validated: true,
            scan_checkpoint_validated: false,
        };

        assert!(discovery.current_record().is_some());
        assert!(matches!(
            discovery.reconcile_once(),
            Err(RpcDiscoveryErrorV1::Rpc(PublicRpcError::Transport(_)))
        ));
        assert!(discovery.current_record().is_none());
    }
}
