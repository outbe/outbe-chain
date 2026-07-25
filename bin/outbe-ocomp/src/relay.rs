use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use alloy_consensus::TxLegacy;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use outbe_node::ocomp::finality::{
    FinalizedIntentVerifier, PublicExactBlockProofSourceV1, PublicFinalizedIntentProofBuildError,
    PublicFinalizedIntentProofBuilderV1, TrieHistoricalCommitteeAuthority,
};
pub use outbe_node::ocomp::finality::{
    PublicAccountProofV1, PublicBlockViewV1, PublicFinalizationBytesV1, PublicStorageProofV1,
};
use outbe_ocomp_protocol::{
    abi::GET_OFFCHAIN_JOB_SELECTOR,
    activation::{encode_activate_lysis_calldata, CandidateAnnouncementV1, PoCActivationV1},
    certificate::build_execution_certificate,
    committee::{OcompCommitteeSnapshotV1, POC_COMMITTEE_THRESHOLD},
    intent::{
        ExpectedFinalizedIntentBindingV1, FinalizedIntentProofV1, FinalizedIntentVerificationError,
        JobIntentV1,
    },
    result::LysisResultV1,
    ProtocolError, SchemaLimits,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    signer::{OutbeEvmSigner, SignerError},
};
use thiserror::Error;

pub struct PublicExactBlockRpcClientV1 {
    endpoint: String,
    client: reqwest::blocking::Client,
    next_id: AtomicU64,
    max_response_bytes: usize,
}

impl PublicExactBlockRpcClientV1 {
    pub fn new(
        endpoint: impl Into<String>,
        max_response_bytes: usize,
    ) -> Result<Self, PublicRpcError> {
        if max_response_bytes == 0 {
            return Err(PublicRpcError::InvalidConfiguration(
                "response byte cap must be non-zero",
            ));
        }
        let endpoint = endpoint.into();
        reqwest::Url::parse(&endpoint)
            .map_err(|error| PublicRpcError::Transport(error.to_string()))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| PublicRpcError::Transport(error.to_string()))?;
        Ok(Self {
            endpoint,
            client,
            next_id: AtomicU64::new(1),
            max_response_bytes,
        })
    }

    pub fn finalization(&self, height: u64) -> Result<PublicFinalizationBytesV1, PublicRpcError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Response {
            finalization_hex: String,
            block_hex: String,
        }

        let result = self.call("outbe_getFinalization", serde_json::json!([height]))?;
        let response: Response = serde_json::from_value(result)
            .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?;
        Ok(PublicFinalizationBytesV1 {
            finalization_bytes: decode_rpc_hex(
                &response.finalization_hex,
                self.max_response_bytes,
            )?,
            block_bytes: decode_rpc_hex(&response.block_hex, self.max_response_bytes)?,
        })
    }

    pub fn block_by_hash(&self, block_hash: B256) -> Result<PublicBlockViewV1, PublicRpcError> {
        let result = self.call(
            "eth_getBlockByHash",
            serde_json::json!([format!("{block_hash:#x}"), false]),
        )?;
        let response_hash = parse_b256_field(&result, "hash")?;
        if response_hash != block_hash {
            return Err(PublicRpcError::AuthorityMismatch("block hash"));
        }
        Ok(PublicBlockViewV1 {
            hash: response_hash,
            state_root: parse_b256_field(&result, "stateRoot")?,
            number: parse_hex_u64_field(&result, "number")?,
        })
    }

    pub fn block_number(&self) -> Result<u64, PublicRpcError> {
        let result = self.call("eth_blockNumber", serde_json::json!([]))?;
        let encoded = result.as_str().ok_or_else(|| {
            PublicRpcError::MalformedResponse("block number is not a hex quantity".to_owned())
        })?;
        parse_rpc_u64(encoded, "block number")
    }

    pub fn account_proof(
        &self,
        address: Address,
        storage_slots: &[B256],
        block_hash: B256,
    ) -> Result<PublicAccountProofV1, PublicRpcError> {
        let result = self.call(
            "eth_getProof",
            serde_json::json!([
                format!("{address:#x}"),
                storage_slots
                    .iter()
                    .map(|slot| format!("{slot:#x}"))
                    .collect::<Vec<_>>(),
                {
                    "blockHash": format!("{block_hash:#x}"),
                    "requireCanonical": true
                }
            ]),
        )?;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RpcStorageProofV1 {
            key: String,
            value: String,
            proof: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RpcAccountProofV1 {
            address: String,
            balance: String,
            code_hash: String,
            nonce: String,
            storage_hash: String,
            account_proof: Vec<String>,
            storage_proof: Vec<RpcStorageProofV1>,
        }
        let response: RpcAccountProofV1 = serde_json::from_value(result)
            .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?;
        let response_address = Address::from_str(&response.address)
            .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?;
        if response_address != address {
            return Err(PublicRpcError::AuthorityMismatch("proof address"));
        }
        let account_nodes = response
            .account_proof
            .iter()
            .map(|node| decode_rpc_hex(node, self.max_response_bytes).map(Bytes::from))
            .collect::<Result<Vec<_>, _>>()?;
        let storage_proofs = response
            .storage_proof
            .into_iter()
            .map(|proof| {
                Ok(PublicStorageProofV1 {
                    key: parse_rpc_word(&proof.key, "storage proof key")?,
                    value: parse_rpc_u256(&proof.value, "storage proof value")?,
                    nodes: proof
                        .proof
                        .iter()
                        .map(|node| decode_rpc_hex(node, self.max_response_bytes).map(Bytes::from))
                        .collect::<Result<Vec<_>, PublicRpcError>>()?,
                })
            })
            .collect::<Result<Vec<_>, PublicRpcError>>()?;
        Ok(PublicAccountProofV1 {
            address: response_address,
            nonce: parse_rpc_u64(&response.nonce, "account nonce")?,
            balance: parse_rpc_u256(&response.balance, "account balance")?,
            storage_root: B256::from_str(&response.storage_hash)
                .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?,
            code_hash: B256::from_str(&response.code_hash)
                .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?,
            account_nodes,
            storage_proofs,
        })
    }

    pub fn job_record(&self, intent_id: B256, block_hash: B256) -> Result<Vec<u8>, PublicRpcError> {
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&GET_OFFCHAIN_JOB_SELECTOR);
        calldata.extend_from_slice(intent_id.as_slice());
        let result = self.call(
            "eth_call",
            serde_json::json!([
                {
                    "to": format!("{METADOSIS_ADDRESS:#x}"),
                    "data": format!("0x{}", hex::encode(calldata))
                },
                {
                    "blockHash": format!("{block_hash:#x}"),
                    "requireCanonical": true
                }
            ]),
        )?;
        let encoded = result.as_str().ok_or_else(|| {
            PublicRpcError::MalformedResponse(
                "eth_call job record result is not hex bytes".to_owned(),
            )
        })?;
        decode_abi_bytes_return(encoded, self.max_response_bytes)
    }

    pub fn verified_relay_job(
        &self,
        request_height: u64,
        intent_id: B256,
        expected: ExpectedFinalizedIntentBindingV1,
        committee: OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: SchemaLimits,
    ) -> Result<VerifiedRelayJobV1, RelayJobLoadError> {
        let (proof, verified) = PublicFinalizedIntentProofBuilderV1::new(self, limits)
            .build_and_verify(request_height, intent_id, expected)?;
        let verified_binding = ExpectedFinalizedIntentBindingV1 {
            chain_id: verified.intent.chain_id,
            genesis_hash: verified.intent.genesis_hash,
            fork_id: verified.intent.fork_id,
            protocol_bundle_hash: verified.intent.protocol_bundle_hash,
        };
        VerifiedRelayJobV1::verify(proof, verified_binding, committee, current_height, limits)
            .map_err(Into::into)
    }

    fn call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PublicRpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });
        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| PublicRpcError::Transport(error.to_string()))?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(PublicRpcError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }
        let read_limit = u64::try_from(self.max_response_bytes)
            .map_err(|_| PublicRpcError::InvalidConfiguration("response cap does not fit u64"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.max_response_bytes.min(8 * 1_024))
            .map_err(|_| PublicRpcError::AllocationFailed)?;
        response
            .take(read_limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| PublicRpcError::Transport(error.to_string()))?;
        if bytes.len() > self.max_response_bytes {
            return Err(PublicRpcError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }

        #[derive(serde::Deserialize)]
        struct RpcErrorV1 {
            code: i64,
            message: String,
        }
        #[derive(serde::Deserialize)]
        struct RpcResponseV1 {
            jsonrpc: String,
            id: u64,
            result: Option<serde_json::Value>,
            error: Option<RpcErrorV1>,
        }
        let envelope: RpcResponseV1 = serde_json::from_slice(&bytes)
            .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))?;
        if envelope.jsonrpc != "2.0" || envelope.id != id {
            return Err(PublicRpcError::AuthorityMismatch("JSON-RPC envelope"));
        }
        if let Some(error) = envelope.error {
            return Err(PublicRpcError::Remote {
                code: error.code,
                message: error.message,
            });
        }
        envelope.result.ok_or(PublicRpcError::MalformedResponse(
            "missing JSON-RPC result".to_owned(),
        ))
    }
}

#[derive(Debug, Error)]
pub enum RelayJobLoadError {
    #[error(transparent)]
    PublicProof(#[from] PublicFinalizedIntentProofBuildError),
    #[error(transparent)]
    Relay(#[from] RelayError),
}

impl PublicExactBlockProofSourceV1 for PublicExactBlockRpcClientV1 {
    type Error = PublicRpcError;

    fn finalization(&self, height: u64) -> Result<PublicFinalizationBytesV1, Self::Error> {
        Self::finalization(self, height)
    }

    fn block_by_hash(&self, block_hash: B256) -> Result<PublicBlockViewV1, Self::Error> {
        Self::block_by_hash(self, block_hash)
    }

    fn job_record(&self, intent_id: B256, block_hash: B256) -> Result<Vec<u8>, Self::Error> {
        Self::job_record(self, intent_id, block_hash)
    }

    fn account_proof(
        &self,
        address: Address,
        storage_slots: &[B256],
        block_hash: B256,
    ) -> Result<PublicAccountProofV1, Self::Error> {
        Self::account_proof(self, address, storage_slots, block_hash)
    }
}

pub struct NormalActivationSubmitterV1 {
    rpc: PublicExactBlockRpcClientV1,
    payer: OutbeEvmSigner,
    prepared: Mutex<Option<PreparedActivationTransactionV1>>,
}

struct PreparedActivationTransactionV1 {
    calldata: Vec<u8>,
    raw: Vec<u8>,
    expected_hash: B256,
}

pub trait ActivationPublisherV1: Send + Sync {
    fn publish(&self, activation: &PoCActivationV1, limits: &SchemaLimits) -> Result<B256, String>;
}

pub trait RelayHeightSourceV1: Send + Sync {
    fn current_height(&self) -> Result<u64, String>;
}

impl RelayHeightSourceV1 for PublicExactBlockRpcClientV1 {
    fn current_height(&self) -> Result<u64, String> {
        self.block_number().map_err(|error| error.to_string())
    }
}

impl ActivationPublisherV1 for NormalActivationSubmitterV1 {
    fn publish(&self, activation: &PoCActivationV1, limits: &SchemaLimits) -> Result<B256, String> {
        self.submit(activation, limits)
            .map_err(|error| error.to_string())
    }
}

impl NormalActivationSubmitterV1 {
    #[must_use]
    pub fn new(rpc: PublicExactBlockRpcClientV1, payer: OutbeEvmSigner) -> Self {
        Self {
            rpc,
            payer,
            prepared: Mutex::new(None),
        }
    }

    pub fn submit(
        &self,
        activation: &PoCActivationV1,
        limits: &SchemaLimits,
    ) -> Result<B256, ActivationSubmissionError> {
        let calldata = encode_activate_lysis_calldata(activation, limits)?;
        let mut prepared = self
            .prepared
            .lock()
            .map_err(|_| ActivationSubmissionError::PreparedTransactionLockPoisoned)?;
        if let Some(prepared) = prepared.as_ref() {
            if prepared.calldata != calldata {
                return Err(ActivationSubmissionError::DifferentActivationAfterPreparation);
            }
            return self.broadcast(prepared);
        }

        let transaction = self.prepare(calldata)?;
        *prepared = Some(transaction);
        self.broadcast(prepared.as_ref().expect("prepared transaction was stored"))
    }

    fn prepare(
        &self,
        calldata: Vec<u8>,
    ) -> Result<PreparedActivationTransactionV1, ActivationSubmissionError> {
        let payer = self.payer.address();
        let chain_id = parse_rpc_quantity_u64(
            self.rpc.call("eth_chainId", serde_json::json!([]))?,
            "chain id",
        )?;
        let nonce = parse_rpc_quantity_u64(
            self.rpc.call(
                "eth_getTransactionCount",
                serde_json::json!([format!("{payer:#x}"), "pending"]),
            )?,
            "pending nonce",
        )?;
        let gas_price = parse_rpc_quantity_u128(
            self.rpc.call("eth_gasPrice", serde_json::json!([]))?,
            "gas price",
        )?;
        let estimated_gas = parse_rpc_quantity_u64(
            self.rpc.call(
                "eth_estimateGas",
                serde_json::json!([{
                    "from": format!("{payer:#x}"),
                    "to": format!("{METADOSIS_ADDRESS:#x}"),
                    "data": format!("0x{}", hex::encode(&calldata)),
                    "value": "0x0"
                }]),
            )?,
            "estimated gas",
        )?;
        let gas_limit = estimated_gas
            .checked_add(estimated_gas / 5)
            .ok_or(ActivationSubmissionError::GasLimitOverflow)?;
        let unsigned = TxLegacy {
            chain_id: Some(chain_id),
            nonce,
            gas_price,
            gas_limit,
            to: TxKind::Call(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input: Bytes::copy_from_slice(&calldata),
        };
        let signed = self.payer.sign_unsigned(unsigned)?;
        let expected_hash = *signed.hash();
        let mut raw = Vec::new();
        raw.try_reserve_exact(signed.encode_2718_len())
            .map_err(|_| ActivationSubmissionError::AllocationFailed)?;
        signed.encode_2718(&mut raw);
        Ok(PreparedActivationTransactionV1 {
            calldata,
            raw,
            expected_hash,
        })
    }

    fn broadcast(
        &self,
        prepared: &PreparedActivationTransactionV1,
    ) -> Result<B256, ActivationSubmissionError> {
        let submitted_hash = parse_rpc_b256(
            self.rpc.call(
                "eth_sendRawTransaction",
                serde_json::json!([format!("0x{}", hex::encode(&prepared.raw))]),
            )?,
            "submitted transaction hash",
        )?;
        if submitted_hash != prepared.expected_hash {
            return Err(ActivationSubmissionError::TransactionHashMismatch {
                expected: prepared.expected_hash,
                actual: submitted_hash,
            });
        }
        Ok(submitted_hash)
    }
}

fn parse_rpc_quantity_u64(
    value: serde_json::Value,
    field: &'static str,
) -> Result<u64, ActivationSubmissionError> {
    parse_rpc_quantity(&value, field).and_then(|digits| {
        u64::from_str_radix(digits, 16)
            .map_err(|_| ActivationSubmissionError::InvalidRpcQuantity { field })
    })
}

fn parse_rpc_quantity_u128(
    value: serde_json::Value,
    field: &'static str,
) -> Result<u128, ActivationSubmissionError> {
    parse_rpc_quantity(&value, field).and_then(|digits| {
        u128::from_str_radix(digits, 16)
            .map_err(|_| ActivationSubmissionError::InvalidRpcQuantity { field })
    })
}

fn parse_rpc_quantity<'a>(
    value: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, ActivationSubmissionError> {
    let encoded = value
        .as_str()
        .ok_or(ActivationSubmissionError::InvalidRpcQuantity { field })?;
    let digits = encoded
        .strip_prefix("0x")
        .ok_or(ActivationSubmissionError::InvalidRpcQuantity { field })?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ActivationSubmissionError::InvalidRpcQuantity { field });
    }
    Ok(digits)
}

fn parse_rpc_b256(
    value: serde_json::Value,
    field: &'static str,
) -> Result<B256, ActivationSubmissionError> {
    let encoded = value
        .as_str()
        .ok_or(ActivationSubmissionError::InvalidRpcHash { field })?;
    B256::from_str(encoded).map_err(|_| ActivationSubmissionError::InvalidRpcHash { field })
}

#[derive(Debug, Error)]
pub enum ActivationSubmissionError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Rpc(#[from] PublicRpcError),
    #[error(transparent)]
    Signer(#[from] SignerError),
    #[error("invalid public RPC quantity for {field}")]
    InvalidRpcQuantity { field: &'static str },
    #[error("invalid public RPC hash for {field}")]
    InvalidRpcHash { field: &'static str },
    #[error("estimated gas plus the 20% safety margin overflows u64")]
    GasLimitOverflow,
    #[error("activation transaction allocation failed")]
    AllocationFailed,
    #[error("activation transaction cache lock is poisoned")]
    PreparedTransactionLockPoisoned,
    #[error("a different activation was offered after transaction bytes were prepared")]
    DifferentActivationAfterPreparation,
    #[error(
        "public RPC returned transaction hash {actual}, expected locally signed hash {expected}"
    )]
    TransactionHashMismatch { expected: B256, actual: B256 },
}

fn decode_rpc_hex(value: &str, limit: usize) -> Result<Vec<u8>, PublicRpcError> {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    if !hex.len().is_multiple_of(2) || hex.len() / 2 > limit {
        return Err(PublicRpcError::MalformedResponse(
            "invalid or over-cap hex bytes".to_owned(),
        ));
    }
    hex::decode(hex).map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
}

fn parse_rpc_u64(value: &str, field: &'static str) -> Result<u64, PublicRpcError> {
    let digits = value.strip_prefix("0x").ok_or_else(|| {
        PublicRpcError::MalformedResponse(format!("{field} is not a hex quantity"))
    })?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return Err(PublicRpcError::MalformedResponse(format!(
            "{field} is not a canonical hex quantity"
        )));
    }
    u64::from_str_radix(digits, 16)
        .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
}

fn parse_rpc_u256(value: &str, field: &'static str) -> Result<U256, PublicRpcError> {
    let digits = value.strip_prefix("0x").ok_or_else(|| {
        PublicRpcError::MalformedResponse(format!("{field} is not a hex quantity"))
    })?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return Err(PublicRpcError::MalformedResponse(format!(
            "{field} is not a canonical hex quantity"
        )));
    }
    U256::from_str_radix(digits, 16)
        .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
}

fn parse_rpc_word(value: &str, field: &'static str) -> Result<B256, PublicRpcError> {
    let digits = value
        .strip_prefix("0x")
        .ok_or_else(|| PublicRpcError::MalformedResponse(format!("{field} is not hex")))?;
    if digits.is_empty() || digits.len() > 64 {
        return Err(PublicRpcError::MalformedResponse(format!(
            "{field} does not fit one word"
        )));
    }
    U256::from_str_radix(digits, 16)
        .map(|word| B256::new(word.to_be_bytes::<32>()))
        .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
}

fn decode_abi_bytes_return(value: &str, limit: usize) -> Result<Vec<u8>, PublicRpcError> {
    let encoded = decode_rpc_hex(value, limit.saturating_add(64))?;
    if encoded.len() < 64 || encoded.len() % 32 != 0 {
        return Err(PublicRpcError::MalformedResponse(
            "job record ABI bytes have an invalid word shape".to_owned(),
        ));
    }
    let offset = U256::from_be_slice(&encoded[..32]);
    if offset != U256::from(32) {
        return Err(PublicRpcError::MalformedResponse(
            "job record ABI bytes have a non-canonical offset".to_owned(),
        ));
    }
    let length = usize::try_from(U256::from_be_slice(&encoded[32..64]))
        .map_err(|_| PublicRpcError::MalformedResponse("job record length overflow".to_owned()))?;
    if length > limit {
        return Err(PublicRpcError::ResponseTooLarge { limit });
    }
    let padded = length
        .checked_add(31)
        .map(|value| value / 32 * 32)
        .and_then(|value| value.checked_add(64))
        .ok_or_else(|| {
            PublicRpcError::MalformedResponse("job record ABI length overflow".to_owned())
        })?;
    if encoded.len() != padded || encoded[64 + length..].iter().any(|byte| *byte != 0) {
        return Err(PublicRpcError::MalformedResponse(
            "job record ABI bytes are not canonical".to_owned(),
        ));
    }
    Ok(encoded[64..64 + length].to_vec())
}

fn parse_b256_field(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<B256, PublicRpcError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PublicRpcError::MalformedResponse(format!("missing {field}")))
        .and_then(|value| {
            B256::from_str(value)
                .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
        })
}

fn parse_hex_u64_field(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<u64, PublicRpcError> {
    let encoded = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PublicRpcError::MalformedResponse(format!("missing {field}")))?;
    u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16)
        .map_err(|error| PublicRpcError::MalformedResponse(error.to_string()))
}

pub struct VerifiedRelayJobV1 {
    proof: FinalizedIntentProofV1,
    intent: JobIntentV1,
    intent_id: B256,
    job_id: B256,
    request_state_root: B256,
    committee: OcompCommitteeSnapshotV1,
    limits: SchemaLimits,
}

impl VerifiedRelayJobV1 {
    pub fn verify(
        proof: FinalizedIntentProofV1,
        expected: ExpectedFinalizedIntentBindingV1,
        committee: OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: SchemaLimits,
    ) -> Result<Self, RelayError> {
        let verified = proof.verify(
            expected,
            &FinalizedIntentVerifier::new(TrieHistoricalCommitteeAuthority),
            &limits,
        )?;
        let committee_hash = committee.snapshot_hash(&limits)?;
        if verified.intent.result_committee_snapshot_hash != committee_hash {
            return Err(RelayError::Protocol(ProtocolError::InvalidInvariant(
                "relay job committee binding",
            )));
        }
        if current_height >= verified.intent.deadline_height {
            return Err(RelayError::Protocol(ProtocolError::InvalidInvariant(
                "relay job before deadline",
            )));
        }
        Ok(Self {
            proof,
            intent: verified.intent,
            intent_id: verified.intent_id,
            job_id: verified.job_id,
            request_state_root: verified.request.state_root,
            committee,
            limits,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum RelayAcceptOutcomeV1 {
    Accepted,
    ExactDuplicate,
    ActivationReady {
        activation: Box<PoCActivationV1>,
        calldata: Vec<u8>,
    },
}

struct CandidateGroupV1 {
    result_digest: B256,
    canonical_result: Vec<u8>,
    candidates: Vec<CandidateAnnouncementV1>,
}

pub struct CandidateRelayV1 {
    job: VerifiedRelayJobV1,
    announcements_by_validator: BTreeMap<u8, Vec<u8>>,
    groups: Vec<CandidateGroupV1>,
    activation_ready: Option<(PoCActivationV1, Vec<u8>)>,
}

impl CandidateRelayV1 {
    #[must_use]
    pub fn new(job: VerifiedRelayJobV1) -> Self {
        Self {
            job,
            announcements_by_validator: BTreeMap::new(),
            groups: Vec::new(),
            activation_ready: None,
        }
    }

    pub fn accept_candidate(
        &mut self,
        canonical_announcement: &[u8],
        current_height: u64,
    ) -> Result<RelayAcceptOutcomeV1, RelayError> {
        if current_height >= self.job.intent.deadline_height {
            return Err(RelayError::DeadlineReached {
                current_height,
                deadline_height: self.job.intent.deadline_height,
            });
        }
        if canonical_announcement.len() > self.job.limits.max_control_body_bytes {
            return Err(RelayError::CandidateBodyTooLarge {
                limit: self.job.limits.max_control_body_bytes,
                actual: canonical_announcement.len(),
            });
        }
        let candidate =
            CandidateAnnouncementV1::decode_canonical(canonical_announcement, &self.job.limits)?;
        candidate.verify(
            &self.job.intent,
            self.job.job_id,
            &self.job.committee,
            current_height,
            &self.job.limits,
        )?;

        if let Some(previous) = self
            .announcements_by_validator
            .get(&candidate.validator_index)
        {
            return if previous == canonical_announcement {
                if let Some((activation, calldata)) = &self.activation_ready {
                    activation.verify(
                        self.job.request_state_root,
                        &self.job.committee,
                        current_height,
                        &self.job.limits,
                    )?;
                    Ok(RelayAcceptOutcomeV1::ActivationReady {
                        activation: Box::new(activation.clone()),
                        calldata: calldata.clone(),
                    })
                } else {
                    Ok(RelayAcceptOutcomeV1::ExactDuplicate)
                }
            } else {
                Err(RelayError::ConflictingValidatorAnnouncement {
                    validator_index: candidate.validator_index,
                })
            };
        }

        let canonical_result = candidate.result.encode_canonical(&self.job.limits)?;
        let group_index = self
            .groups
            .iter()
            .position(|group| {
                group.result_digest == candidate.result_digest
                    && group.canonical_result == canonical_result
            })
            .unwrap_or_else(|| {
                self.groups.push(CandidateGroupV1 {
                    result_digest: candidate.result_digest,
                    canonical_result,
                    candidates: Vec::new(),
                });
                self.groups.len() - 1
            });
        self.announcements_by_validator
            .insert(candidate.validator_index, canonical_announcement.to_vec());
        self.groups[group_index].candidates.push(candidate);

        if self.activation_ready.is_some()
            || self.groups[group_index].candidates.len() < usize::from(POC_COMMITTEE_THRESHOLD)
        {
            return Ok(RelayAcceptOutcomeV1::Accepted);
        }

        let group = &self.groups[group_index];
        let certificate = build_execution_certificate(
            &group.candidates,
            &self.job.intent,
            self.job.job_id,
            &self.job.committee,
            current_height,
            &self.job.limits,
        )?;
        let result: LysisResultV1 = group.candidates[0].result.clone();
        let activation = PoCActivationV1 {
            intent_id: self.job.intent_id,
            finalized_intent_proof: self.job.proof.clone(),
            activation_payload: result.activation_payload(&self.job.limits)?,
            result,
            certificate,
        };
        activation.verify(
            self.job.request_state_root,
            &self.job.committee,
            current_height,
            &self.job.limits,
        )?;
        let calldata = encode_activate_lysis_calldata(&activation, &self.job.limits)?;
        self.activation_ready = Some((activation.clone(), calldata.clone()));
        Ok(RelayAcceptOutcomeV1::ActivationReady {
            activation: Box::new(activation),
            calldata,
        })
    }

    #[must_use]
    pub const fn candidate_body_limit(&self) -> usize {
        self.job.limits.max_control_body_bytes
    }
}

pub struct RelayHttpServerV1 {
    listener: TcpListener,
    relay: Mutex<CandidateRelayV1>,
    publisher: Arc<dyn ActivationPublisherV1>,
    height: Arc<dyn RelayHeightSourceV1>,
    published_transaction: Mutex<Option<B256>>,
}

impl RelayHttpServerV1 {
    pub fn bind<P>(
        address: impl ToSocketAddrs,
        relay: CandidateRelayV1,
        publisher: Arc<P>,
        height: Arc<impl RelayHeightSourceV1 + 'static>,
    ) -> io::Result<Self>
    where
        P: ActivationPublisherV1 + 'static,
    {
        Ok(Self {
            listener: TcpListener::bind(address)?,
            relay: Mutex::new(relay),
            publisher,
            height,
            published_transaction: Mutex::new(None),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn serve_one(&self) -> io::Result<()> {
        let (mut stream, _) = self.listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        self.handle_connection(&mut stream)
    }

    pub fn serve(&self) -> io::Result<()> {
        loop {
            if let Err(error) = self.serve_one() {
                if is_peer_connection_error(error.kind()) {
                    continue;
                }
                return Err(error);
            }
        }
    }

    fn handle_connection(&self, stream: &mut TcpStream) -> io::Result<()> {
        const MAX_HEADER_BYTES: usize = 8 * 1_024;
        let header = read_http_header(stream, MAX_HEADER_BYTES)?;
        let Ok(header) = std::str::from_utf8(&header) else {
            return write_http_status(stream, "400 Bad Request");
        };
        let mut lines = header[..header.len() - 4].split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut request_parts = request_line.split(' ');
        let method = request_parts.next().unwrap_or_default();
        let target = request_parts.next().unwrap_or_default();
        let version = request_parts.next().unwrap_or_default();
        if request_parts.next().is_some() || version != "HTTP/1.1" {
            return write_http_status(stream, "400 Bad Request");
        }

        let mut content_length = None;
        let mut content_type = None;
        let mut transfer_encoding = false;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                return write_http_status(stream, "400 Bad Request");
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                if content_length.is_some() {
                    return write_http_status(stream, "400 Bad Request");
                }
                content_length = value.parse::<usize>().ok();
                if content_length.is_none() {
                    return write_http_status(stream, "400 Bad Request");
                }
            } else if name.eq_ignore_ascii_case("content-type") {
                if content_type.replace(value).is_some() {
                    return write_http_status(stream, "400 Bad Request");
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                transfer_encoding = true;
            }
        }
        if transfer_encoding {
            return write_http_status(stream, "400 Bad Request");
        }

        if method == "GET" && target == "/healthz" {
            if content_length.unwrap_or(0) != 0 {
                return write_http_status(stream, "400 Bad Request");
            }
            return write_http_status(stream, "200 OK");
        }
        if method != "POST" || target != "/v1/candidates" {
            return write_http_status(stream, "404 Not Found");
        }
        if !content_type.is_some_and(|value| value.eq_ignore_ascii_case("application/octet-stream"))
        {
            return write_http_status(stream, "415 Unsupported Media Type");
        }
        let Some(content_length) = content_length else {
            return write_http_status(stream, "411 Length Required");
        };
        let body_limit = self
            .relay
            .lock()
            .map_err(|_| io::Error::other("relay lock poisoned"))?
            .candidate_body_limit();
        if content_length > body_limit {
            return write_http_status(stream, "413 Payload Too Large");
        }
        let mut body = Vec::new();
        body.try_reserve_exact(content_length)
            .map_err(|_| io::Error::other("candidate body allocation failed"))?;
        body.resize(content_length, 0);
        if stream.read_exact(&mut body).is_err() {
            return write_http_status(stream, "400 Bad Request");
        }

        let current_height = match self.height.current_height() {
            Ok(height) => height,
            Err(_) => return write_http_status(stream, "503 Service Unavailable"),
        };
        let outcome = self
            .relay
            .lock()
            .map_err(|_| io::Error::other("relay lock poisoned"))?
            .accept_candidate(&body, current_height);
        match outcome {
            Ok(RelayAcceptOutcomeV1::ExactDuplicate) => write_http_status(stream, "200 OK"),
            Ok(RelayAcceptOutcomeV1::Accepted) => write_http_status(stream, "202 Accepted"),
            Ok(RelayAcceptOutcomeV1::ActivationReady { activation, .. }) => {
                let mut published = self
                    .published_transaction
                    .lock()
                    .map_err(|_| io::Error::other("publication lock poisoned"))?;
                if published.is_some() {
                    return write_http_status(stream, "200 OK");
                }
                match self.publisher.publish(
                    &activation,
                    &self
                        .relay
                        .lock()
                        .map_err(|_| io::Error::other("relay lock poisoned"))?
                        .job
                        .limits,
                ) {
                    Ok(transaction_hash) => {
                        *published = Some(transaction_hash);
                        write_http_status(stream, "202 Accepted")
                    }
                    Err(_) => write_http_status(stream, "503 Service Unavailable"),
                }
            }
            Err(RelayError::CandidateBodyTooLarge { .. }) => {
                write_http_status(stream, "413 Payload Too Large")
            }
            Err(RelayError::ConflictingValidatorAnnouncement { .. }) => {
                write_http_status(stream, "409 Conflict")
            }
            Err(RelayError::Protocol(_) | RelayError::FinalizedIntent(_)) => {
                write_http_status(stream, "400 Bad Request")
            }
            Err(RelayError::DeadlineReached { .. }) => write_http_status(stream, "400 Bad Request"),
        }
    }
}

const fn is_peer_connection_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::InvalidData
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
    )
}

fn read_http_header(stream: &mut TcpStream, limit: usize) -> io::Result<Vec<u8>> {
    let mut header = Vec::new();
    header
        .try_reserve_exact(1_024)
        .map_err(|_| io::Error::other("HTTP header allocation failed"))?;
    let mut byte = [0_u8; 1];
    while header.len() < limit {
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            return Ok(header);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTP header exceeds cap",
    ))
}

fn write_http_status(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

#[derive(Debug, Error)]
pub enum PublicRpcError {
    #[error("invalid public RPC configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("public RPC transport failed: {0}")]
    Transport(String),
    #[error("public RPC response exceeds {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("public RPC response allocation failed")]
    AllocationFailed,
    #[error("public RPC response is malformed: {0}")]
    MalformedResponse(String),
    #[error("public RPC authority mismatch: {0}")]
    AuthorityMismatch(&'static str),
    #[error("public RPC returned error {code}: {message}")]
    Remote { code: i64, message: String },
}

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("candidate body exceeds cap: {actual} > {limit}")]
    CandidateBodyTooLarge { limit: usize, actual: usize },
    #[error("validator {validator_index} announced two different results")]
    ConflictingValidatorAnnouncement { validator_index: u8 },
    #[error(
        "relay job deadline reached: current height {current_height}, exclusive deadline {deadline_height}"
    )]
    DeadlineReached {
        current_height: u64,
        deadline_height: u64,
    },
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    FinalizedIntent(#[from] FinalizedIntentVerificationError),
}
