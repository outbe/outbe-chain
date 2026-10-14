use std::{
    io::Read,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use alloy_primitives::{Address, B256};
use thiserror::Error;

use super::{VoteBlockV1, VoteReceiptV1, VoteSubmissionRpcV1};

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PublicVoteRpcClientV1 {
    endpoint: String,
    client: reqwest::blocking::Client,
    next_id: AtomicU64,
    max_response_bytes: usize,
}

impl PublicVoteRpcClientV1 {
    pub fn new(
        endpoint: impl Into<String>,
        max_response_bytes: usize,
    ) -> Result<Self, PublicVoteRpcErrorV1> {
        if max_response_bytes == 0 {
            return Err(PublicVoteRpcErrorV1::InvalidConfiguration);
        }
        let endpoint = endpoint.into();
        reqwest::Url::parse(&endpoint)
            .map_err(|error| PublicVoteRpcErrorV1::Transport(error.to_string()))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(RPC_TIMEOUT)
            .build()
            .map_err(|error| PublicVoteRpcErrorV1::Transport(error.to_string()))?;
        Ok(Self {
            endpoint,
            client,
            next_id: AtomicU64::new(1),
            max_response_bytes,
        })
    }

    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PublicVoteRpcErrorV1> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
                "id": id,
            }))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| PublicVoteRpcErrorV1::Transport(error.to_string()))?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(PublicVoteRpcErrorV1::ResponseTooLarge);
        }
        let read_limit = u64::try_from(self.max_response_bytes)
            .map_err(|_| PublicVoteRpcErrorV1::InvalidConfiguration)?;
        let mut bytes = Vec::new();
        response
            .take(read_limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| PublicVoteRpcErrorV1::Transport(error.to_string()))?;
        if bytes.len() > self.max_response_bytes {
            return Err(PublicVoteRpcErrorV1::ResponseTooLarge);
        }
        parse_rpc_response(&bytes, id)
    }

    fn block_for_tag(&self, tag: serde_json::Value) -> Result<VoteBlockV1, PublicVoteRpcErrorV1> {
        let value = self.call("eth_getBlockByNumber", serde_json::json!([tag, false]))?;
        if value.is_null() {
            return Err(PublicVoteRpcErrorV1::MissingBlock);
        }
        Ok(VoteBlockV1 {
            number: parse_quantity_field(&value, "number")?,
            hash: parse_b256_field(&value, "hash")?,
        })
    }
}

pub(super) fn parse_rpc_response(
    bytes: &[u8],
    expected_id: u64,
) -> Result<serde_json::Value, PublicVoteRpcErrorV1> {
    #[derive(serde::Deserialize)]
    struct RpcError {
        code: i64,
        message: String,
    }

    let envelope: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| PublicVoteRpcErrorV1::Malformed(error.to_string()))?;
    if envelope.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || envelope.get("id").and_then(serde_json::Value::as_u64) != Some(expected_id)
    {
        return Err(PublicVoteRpcErrorV1::Malformed(
            "JSON-RPC envelope mismatch".to_owned(),
        ));
    }
    if let Some(error) = envelope.get("error").filter(|error| !error.is_null()) {
        let error: RpcError = serde_json::from_value(error.clone())
            .map_err(|error| PublicVoteRpcErrorV1::Malformed(error.to_string()))?;
        return Err(PublicVoteRpcErrorV1::Remote {
            code: error.code,
            message: error.message,
        });
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| PublicVoteRpcErrorV1::Malformed("missing result".to_owned()))
}

impl VoteSubmissionRpcV1 for PublicVoteRpcClientV1 {
    type Error = PublicVoteRpcErrorV1;

    fn chain_id(&self) -> Result<u64, Self::Error> {
        parse_quantity(self.call("eth_chainId", serde_json::json!([]))?, "chain id")
    }

    fn canonical_nonce(&self, address: Address) -> Result<u64, Self::Error> {
        parse_quantity(
            self.call("eth_getTransactionCount", canonical_nonce_params(address))?,
            "canonical nonce",
        )
    }

    fn gas_price(&self) -> Result<u128, Self::Error> {
        parse_quantity_u128(
            self.call("eth_gasPrice", serde_json::json!([]))?,
            "gas price",
        )
    }

    fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
        expected_hash: B256,
    ) -> Result<B256, Self::Error> {
        match self.call(
            "eth_sendRawTransaction",
            serde_json::json!([format!("0x{}", hex::encode(raw_transaction))]),
        ) {
            Ok(value) => parse_b256(value, "submitted transaction hash"),
            Err(PublicVoteRpcErrorV1::Remote { message, .. })
                if message.to_ascii_lowercase().contains("already known")
                    || message.to_ascii_lowercase().contains("known transaction") =>
            {
                Ok(expected_hash)
            }
            Err(error) => Err(error),
        }
    }

    fn transaction_receipt(
        &self,
        transaction_hash: B256,
    ) -> Result<Option<VoteReceiptV1>, Self::Error> {
        let value = self.call(
            "eth_getTransactionReceipt",
            serde_json::json!([format!("{transaction_hash:#x}")]),
        )?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(VoteReceiptV1 {
            transaction_hash: parse_b256_field(&value, "transactionHash")?,
            block_number: parse_quantity_field(&value, "blockNumber")?,
            block_hash: parse_b256_field(&value, "blockHash")?,
            success: parse_quantity_field(&value, "status")? == 1,
        }))
    }

    fn canonical_block(&self, number: u64) -> Result<Option<VoteBlockV1>, Self::Error> {
        let value = self.call(
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{number:x}"), false]),
        )?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(VoteBlockV1 {
            number: parse_quantity_field(&value, "number")?,
            hash: parse_b256_field(&value, "hash")?,
        }))
    }

    fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error> {
        self.block_for_tag(serde_json::Value::String("finalized".to_owned()))
    }
}

pub(super) fn canonical_nonce_params(address: Address) -> serde_json::Value {
    serde_json::json!([format!("{address:#x}"), "latest"])
}

fn parse_quantity_digits<'a>(
    value: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, PublicVoteRpcErrorV1> {
    let encoded = value
        .as_str()
        .ok_or(PublicVoteRpcErrorV1::InvalidQuantity(field))?;
    let digits = encoded
        .strip_prefix("0x")
        .ok_or(PublicVoteRpcErrorV1::InvalidQuantity(field))?;
    if digits.is_empty() {
        return Err(PublicVoteRpcErrorV1::InvalidQuantity(field));
    }
    Ok(digits)
}

fn parse_quantity(
    value: serde_json::Value,
    field: &'static str,
) -> Result<u64, PublicVoteRpcErrorV1> {
    u64::from_str_radix(parse_quantity_digits(&value, field)?, 16)
        .map_err(|_| PublicVoteRpcErrorV1::InvalidQuantity(field))
}

fn parse_quantity_u128(
    value: serde_json::Value,
    field: &'static str,
) -> Result<u128, PublicVoteRpcErrorV1> {
    u128::from_str_radix(parse_quantity_digits(&value, field)?, 16)
        .map_err(|_| PublicVoteRpcErrorV1::InvalidQuantity(field))
}

fn parse_quantity_field(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<u64, PublicVoteRpcErrorV1> {
    parse_quantity(
        value
            .get(field)
            .cloned()
            .ok_or_else(|| PublicVoteRpcErrorV1::Malformed(format!("missing {field}")))?,
        field,
    )
}

fn parse_b256(value: serde_json::Value, field: &'static str) -> Result<B256, PublicVoteRpcErrorV1> {
    value
        .as_str()
        .ok_or_else(|| PublicVoteRpcErrorV1::Malformed(format!("{field} is not a string")))
        .and_then(|encoded| {
            B256::from_str(encoded)
                .map_err(|error| PublicVoteRpcErrorV1::Malformed(error.to_string()))
        })
}

fn parse_b256_field(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<B256, PublicVoteRpcErrorV1> {
    parse_b256(
        value
            .get(field)
            .cloned()
            .ok_or_else(|| PublicVoteRpcErrorV1::Malformed(format!("missing {field}")))?,
        field,
    )
}

#[derive(Debug, Error)]
pub enum PublicVoteRpcErrorV1 {
    #[error("invalid public vote RPC configuration")]
    InvalidConfiguration,
    #[error("public vote RPC transport failed: {0}")]
    Transport(String),
    #[error("public vote RPC response exceeds its byte cap")]
    ResponseTooLarge,
    #[error("public vote RPC response is malformed: {0}")]
    Malformed(String),
    #[error("public vote RPC returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("public vote RPC returned no block")]
    MissingBlock,
    #[error("public vote RPC field {0} is not a canonical hex quantity")]
    InvalidQuantity(&'static str),
}

impl PublicVoteRpcErrorV1 {
    pub(super) const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Remote { .. } | Self::MissingBlock
        )
    }
}
