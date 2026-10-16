//! Bounded blocking JSON-RPC transport for the standalone OCOMP service.
//!
//! Job discovery reads the node's finalized state, while purpose-bound input
//! RPCs return exact request-state material that the caller checks against its
//! durable finalized `JobIntent` binding.

use std::{
    io::Read as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use alloy_primitives::B256;
use alloy_sol_types::SolCall as _;
use outbe_metadosis::precompile::IMetadosis;
use serde_json::{json, Value};
use thiserror::Error;

const RPC_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedRpcBlockV1 {
    pub number: u64,
    pub hash: B256,
    pub state_root: B256,
}

pub struct PublicOcompRpcClientV1 {
    url: String,
    client: reqwest::blocking::Client,
    next_id: AtomicU64,
    max_response_bytes: usize,
}

impl PublicOcompRpcClientV1 {
    pub fn tribute_state_source(
        &self,
        subject: &outbe_tee::tribute_source::TributeSourceSubject,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<outbe_tee::tribute_source::TributeStateSource, PublicRpcError> {
        let value = self.call("outbe_getTributeStateSource", json!([subject, id]))?;
        serde_json::from_value(value)
            .map_err(|error| PublicRpcError::malformed("outbe_getTributeStateSource", error))
    }

    pub fn tribute_day_state_proof(
        &self,
        subject: &outbe_tee::tribute_source::TributeSourceSubject,
        day_index: Option<u32>,
    ) -> Result<outbe_tribute::state_proof::TributeDayStateProof, PublicRpcError> {
        let value = self.call("outbe_getTributeDayStateProof", json!([subject, day_index]))?;
        serde_json::from_value(value)
            .map_err(|error| PublicRpcError::malformed("outbe_getTributeDayStateProof", error))
    }

    pub fn new(url: impl Into<String>, max_response_bytes: usize) -> Result<Self, PublicRpcError> {
        if max_response_bytes == 0 {
            return Err(PublicRpcError::InvalidResponseLimit);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(RPC_TIMEOUT)
            .build()
            .map_err(PublicRpcError::transport)?;
        Ok(Self {
            url: url.into(),
            client,
            next_id: AtomicU64::new(1),
            max_response_bytes,
        })
    }

    pub fn finalized_block(&self) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
        parse_block(self.call("eth_getBlockByNumber", json!(["finalized", false]))?)
    }

    pub fn block_by_number(&self, number: u64) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
        parse_block(self.call(
            "eth_getBlockByNumber",
            json!([format!("0x{number:x}"), false]),
        )?)
    }

    pub fn job_record_at_hash(
        &self,
        intent_id: B256,
        block_hash: B256,
    ) -> Result<Vec<u8>, PublicRpcError> {
        self.job_record_at_tag(intent_id, exact_block_hash_tag(block_hash))
    }

    pub fn lysis_openings(
        &self,
        intent_id: B256,
        canonical_request: &[u8],
    ) -> Result<Vec<u8>, PublicRpcError> {
        let value = self.call(
            "outbe_getOcompLysisOpeningsV1",
            json!([
                format!("{intent_id:#x}"),
                format!("0x{}", hex::encode(canonical_request))
            ]),
        )?;
        parse_hex_bytes(&value, "outbe_getOcompLysisOpeningsV1")
    }

    fn job_record_at_tag(
        &self,
        intent_id: B256,
        block_tag: Value,
    ) -> Result<Vec<u8>, PublicRpcError> {
        let call = IMetadosis::getOffchainJobCall {
            intentId: intent_id,
        };
        let raw = self.call(
            "eth_call",
            json!([{
                "to": format!("{:#x}", outbe_primitives::addresses::METADOSIS_ADDRESS),
                "data": format!("0x{}", hex::encode(call.abi_encode())),
            }, block_tag]),
        )?;
        let raw = parse_hex_bytes(&raw, "eth_call")?;
        IMetadosis::getOffchainJobCall::abi_decode_returns(&raw)
            .map(|decoded| decoded.to_vec())
            .map_err(|error| PublicRpcError::malformed("eth_call", error))
    }

    pub fn call(&self, method: &'static str, params: Value) -> Result<Value, PublicRpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(&self.url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(PublicRpcError::transport)?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(PublicRpcError::ResponseTooLarge);
        }
        let read_cap = self
            .max_response_bytes
            .checked_add(1)
            .ok_or(PublicRpcError::ResponseTooLarge)?;
        let mut body = Vec::new();
        response
            .take(read_cap as u64)
            .read_to_end(&mut body)
            .map_err(PublicRpcError::transport)?;
        if body.len() > self.max_response_bytes {
            return Err(PublicRpcError::ResponseTooLarge);
        }
        let envelope: Value = serde_json::from_slice(&body)
            .map_err(|error| PublicRpcError::malformed(method, error))?;
        if envelope.get("id") != Some(&Value::from(id)) {
            return Err(PublicRpcError::ResponseId);
        }
        if let Some(error) = envelope.get("error") {
            return Err(PublicRpcError::Remote {
                method,
                detail: error.to_string(),
            });
        }
        envelope
            .get("result")
            .cloned()
            .ok_or(PublicRpcError::MissingResult(method))
    }
}

fn exact_block_hash_tag(block_hash: B256) -> Value {
    json!({
        "blockHash": format!("{block_hash:#x}"),
        "requireCanonical": true,
    })
}

fn parse_block(value: Value) -> Result<FinalizedRpcBlockV1, PublicRpcError> {
    if value.is_null() {
        return Err(PublicRpcError::MissingBlock);
    }
    Ok(FinalizedRpcBlockV1 {
        number: parse_hex_u64(field(&value, "number", "block")?, "block number")?,
        hash: parse_b256(field(&value, "hash", "block")?, "block hash")?,
        state_root: parse_b256(field(&value, "stateRoot", "block")?, "block state root")?,
    })
}

fn field<'a>(
    value: &'a Value,
    name: &str,
    method: &'static str,
) -> Result<&'a Value, PublicRpcError> {
    value
        .get(name)
        .ok_or_else(|| malformed(method, &format!("missing {name}")))
}

fn parse_hex_bytes(value: &Value, method: &'static str) -> Result<Vec<u8>, PublicRpcError> {
    let encoded = value
        .as_str()
        .ok_or_else(|| malformed(method, "hex value is not a string"))?;
    hex::decode(encoded.strip_prefix("0x").unwrap_or(encoded))
        .map_err(|error| PublicRpcError::malformed(method, error))
}

fn parse_b256(value: &Value, what: &'static str) -> Result<B256, PublicRpcError> {
    value
        .as_str()
        .ok_or_else(|| malformed("RPC", what))?
        .parse()
        .map_err(|error: alloy_primitives::hex::FromHexError| {
            PublicRpcError::malformed("RPC", error)
        })
}

fn parse_hex_u64(value: &Value, what: &'static str) -> Result<u64, PublicRpcError> {
    let encoded = value.as_str().ok_or_else(|| malformed("RPC", what))?;
    u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16)
        .map_err(|error| PublicRpcError::malformed("RPC", error))
}

fn malformed(method: &'static str, detail: &str) -> PublicRpcError {
    PublicRpcError::malformed(method, detail)
}

#[derive(Debug, Error)]
pub enum PublicRpcError {
    #[error("public OCOMP RPC response limit must be non-zero")]
    InvalidResponseLimit,
    #[error("public OCOMP RPC transport failed: {0}")]
    Transport(String),
    #[error("public OCOMP RPC response exceeded the configured bound")]
    ResponseTooLarge,
    #[error("public OCOMP RPC response id mismatch")]
    ResponseId,
    #[error("public OCOMP RPC {method} returned an error: {detail}")]
    Remote {
        method: &'static str,
        detail: String,
    },
    #[error("public OCOMP RPC {0} response has no result")]
    MissingResult(&'static str),
    #[error("public OCOMP RPC returned no block")]
    MissingBlock,
    #[error("public OCOMP RPC {method} response is malformed: {detail}")]
    Malformed {
        method: &'static str,
        detail: String,
    },
}

impl outbe_tee::nod_state_materialization::TributeSourceReader for PublicOcompRpcClientV1 {
    fn read_tribute(
        &mut self,
        subject: &outbe_tee::tribute_source::TributeSourceSubject,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<outbe_tee::tribute_source::TributeStateSource, outbe_tee::TransportError> {
        self.tribute_state_source(subject, id).map_err(|error| {
            outbe_tee::TransportError::EnclaveError(format!(
                "host Tribute source read failed: {error}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    #[test]
    fn finalized_state_reads_use_an_exact_canonical_block_hash() {
        let block_hash = hash(0x77);
        assert_eq!(
            exact_block_hash_tag(block_hash),
            json!({
                "blockHash": format!("{block_hash:#x}"),
                "requireCanonical": true,
            })
        );
    }
}
