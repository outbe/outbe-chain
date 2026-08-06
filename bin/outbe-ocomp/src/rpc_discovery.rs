//! Finalized OCOMP job discovery from the public chain RPC.
//!
//! `OffchainJobRequested` is only an index hint. Authority is reconstructed
//! from the exact request block with the public finalization/proof builder.

use std::path::PathBuf;

use alloy_primitives::{Bytes, LogData, B256};
use alloy_sol_types::SolEvent as _;
use outbe_metadosis::precompile::IMetadosis;
use outbe_node::ocomp::finality::PublicFinalizedIntentProofBuilderV1;
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    intent::ExpectedFinalizedIntentBindingV1,
    state::{OcompJobRecordV1, OcompJobStatus},
    FinalizedJobSpecV1, FinalizedJobSummaryV1, ProtocolError, SchemaLimits,
};
use thiserror::Error;

use crate::{
    control::EndpointIdentity,
    public_rpc::{PublicOcompRpcClientV1, PublicRpcError},
    supervisor::{DiscoveryJournal, DiscoveryOutcome, DiscoveryRecord, SupervisorDiscoveryError},
};

#[derive(Clone, Debug)]
pub struct FinalizedRpcDiscoveryConfigV1 {
    pub rpc_url: String,
    pub rpc_max_response_bytes: usize,
    pub journal_root: PathBuf,
    pub projection_start_block: u64,
    pub identity: EndpointIdentity,
    pub fork_id: B256,
    pub limits: SchemaLimits,
}

pub struct FinalizedRpcDiscoveryV1 {
    config: FinalizedRpcDiscoveryConfigV1,
    rpc: PublicOcompRpcClientV1,
    journal: DiscoveryJournal,
}

impl FinalizedRpcDiscoveryV1 {
    pub fn open(config: FinalizedRpcDiscoveryConfigV1) -> Result<Self, RpcDiscoveryErrorV1> {
        if config.projection_start_block == 0 || config.fork_id.is_zero() {
            return Err(RpcDiscoveryErrorV1::InvalidConfig);
        }
        let rpc =
            PublicOcompRpcClientV1::new(config.rpc_url.clone(), config.rpc_max_response_bytes)?;
        let journal = DiscoveryJournal::open(&config.journal_root, config.limits)?;
        Ok(Self {
            config,
            rpc,
            journal,
        })
    }

    pub fn reconcile_once(&mut self) -> Result<DiscoveryOutcome, RpcDiscoveryErrorV1> {
        let after_cursor = self.journal.record().map_or(
            self.config.projection_start_block.saturating_sub(1),
            |record| record.cursor,
        );
        let finalized_head = self.rpc.finalized_block()?;
        let Some(from_block) = after_cursor.checked_add(1) else {
            return Err(RpcDiscoveryErrorV1::CursorOverflow);
        };
        if from_block > finalized_head.number {
            return Ok(DiscoveryOutcome::NoNewJob);
        }

        let logs = self.rpc.logs(
            outbe_primitives::addresses::METADOSIS_ADDRESS,
            IMetadosis::OffchainJobRequested::SIGNATURE_HASH,
            from_block,
            finalized_head.number,
        )?;
        let mut previous_position = None;
        for value in logs {
            let indexed = decode_request_log(&value)?;
            let position = (indexed.block_number, indexed.log_index);
            if previous_position.is_some_and(|previous| previous >= position) {
                return Err(RpcDiscoveryErrorV1::NonCanonicalLogOrder);
            }
            previous_position = Some(position);
            if indexed.block_number <= after_cursor {
                return Err(RpcDiscoveryErrorV1::NonMonotonicEventCursor);
            }
            let canonical = self.rpc.block_by_number(indexed.block_number)?;
            if canonical.hash != indexed.block_hash {
                return Err(RpcDiscoveryErrorV1::EventBlockHash);
            }

            let current_bytes = self
                .rpc
                .job_record_at_number(indexed.intent_id, finalized_head.number)?;
            let current = OcompJobRecordV1::decode_canonical(&current_bytes, &self.config.limits)?;
            let Some(current_finalized) = current.finalized.as_ref() else {
                continue;
            };
            if current.status != OcompJobStatus::VotingOpen
                || finalized_head.number < current_finalized.open_height
                || finalized_head.number >= current_finalized.deadline_height
            {
                continue;
            }

            let (_, verified) =
                PublicFinalizedIntentProofBuilderV1::new(&self.rpc, self.config.limits)
                    .build_and_verify(
                        indexed.block_number,
                        indexed.intent_id,
                        ExpectedFinalizedIntentBindingV1 {
                            chain_id: self.config.identity.chain_id,
                            genesis_hash: self.config.identity.genesis_hash,
                            fork_id: self.config.fork_id,
                            protocol_bundle_hash: self.config.identity.protocol_bundle_hash,
                        },
                    )
                    .map_err(|error| RpcDiscoveryErrorV1::FinalizedAuthority(error.to_string()))?;

            require_request_binding(
                &indexed,
                &current,
                current_finalized.job_id,
                &verified,
                &self.config.limits,
            )?;
            let spec = FinalizedJobSpecV1 {
                summary: FinalizedJobSummaryV1 {
                    cursor: indexed.block_number,
                    job_id: verified.job_id,
                    intent_id: verified.intent_id,
                    finalized_block_hash: verified.request.block_hash,
                    finalized_state_root: verified.request.state_root,
                    protocol_bundle_hash: verified.intent.protocol_bundle_hash,
                },
                canonical_job_intent: BoundedBytes(
                    verified.intent.encode_canonical(&self.config.limits)?,
                ),
            };
            let record = self.journal.persist(spec)?;
            return Ok(DiscoveryOutcome::Discovered(Box::new(record)));
        }
        Ok(DiscoveryOutcome::NoNewJob)
    }

    pub fn current_record(&self) -> Option<DiscoveryRecord> {
        self.journal.record().cloned()
    }

    #[must_use]
    pub const fn rpc(&self) -> &PublicOcompRpcClientV1 {
        &self.rpc
    }
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

fn require_request_binding(
    indexed: &IndexedJobRequestV1,
    current: &OcompJobRecordV1,
    finalized_job_id: B256,
    verified: &outbe_ocomp_protocol::intent::VerifiedFinalizedIntentV1,
    limits: &SchemaLimits,
) -> Result<(), RpcDiscoveryErrorV1> {
    let intent = &verified.intent;
    if current.intent != *intent
        || verified.intent_id != indexed.intent_id
        || verified.job_id != finalized_job_id
        || verified.request.block_number != indexed.block_number
        || verified.request.block_hash != indexed.block_hash
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
    Ok(())
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
    #[error("finalized OCOMP request log is marked removed")]
    RemovedLog,
    #[error("finalized OCOMP request log is malformed")]
    MalformedLog,
    #[error("finalized OCOMP request log block hash differs from the canonical block")]
    EventBlockHash,
    #[error("public finalized intent authority verification failed: {0}")]
    FinalizedAuthority(String),
    #[error("finalized OCOMP request event does not match the authenticated job")]
    EventAuthorityMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
