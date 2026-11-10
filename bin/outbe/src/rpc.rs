use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{sol, SolCall};
use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const STAKING: Address = address!("000000000000000000000000000000000000EE02");
const VALIDATOR_SET: Address = address!("000000000000000000000000000000000000EE00");
const TEE_REGISTRY: Address = address!("000000000000000000000000000000000000EE0A");

const TEE_POLICY_V1_MINIMUM_LEASE_OFFSET: usize = 180;
const TEE_POLICY_V1_MAXIMUM_LEASE_OFFSET: usize = 188;
const TEE_POLICY_V1_LEASE_PREFIX_LEN: usize = 196;

sol!("../../contracts/precompiles/src/IStaking.sol");
sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/precompiles/src/ITeeRegistryV1.sol");

pub struct RpcClient {
    url: String,
    client: reqwest::Client,
    id: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub number: u64,
    pub hash: String,
    pub timestamp: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct TeeLeasePolicy {
    pub minimum: u64,
    pub maximum: u64,
}

#[derive(Clone, Debug)]
pub struct EpochInfo {
    pub epoch_number: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusStatus {
    #[serde(default)]
    pub connected_peers: u64,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub has_threshold_shares: bool,
    #[serde(default)]
    pub last_finalized_block: u64,
    #[serde(default)]
    pub next_planned_activation_height: u64,
    #[serde(default)]
    pub is_validator: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub enclave: Option<EnclaveStatus>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnclaveStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub offer_key_ready: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ValidatorRecord {
    pub registered: bool,
    pub status: u8,
    pub has_bls_share: bool,
}

impl ConsensusStatus {
    pub fn is_ready_signer(&self, record: &ValidatorRecord) -> bool {
        if self.mode != "signer" || !self.is_active {
            return false;
        }
        if !self.has_threshold_shares || self.connected_peers == 0 {
            return false;
        }
        let Some(enclave) = self.enclave.as_ref() else {
            return false;
        };
        if !enclave.ready || !enclave.offer_key_ready {
            return false;
        }
        record.status == 2 && record.has_bls_share
    }
}

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
    id: u64,
}

#[derive(Deserialize)]
struct Response {
    result: Option<Value>,
    error: Option<RpcError>,
    id: Option<Value>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcClient {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            url: url.into(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()?,
            id: AtomicU64::new(1),
        })
    }

    pub async fn chain_id(&self) -> Result<u64> {
        parse_hex_u64(&self.call("eth_chainId", json!([])).await?)
    }

    pub async fn finalized_block(&self) -> Result<Block> {
        self.block("finalized").await
    }

    pub async fn block_number(&self, number: u64) -> Result<Block> {
        self.block(&format!("0x{number:x}")).await
    }

    pub async fn peer_count(&self) -> Result<u64> {
        parse_hex_u64(&self.call("net_peerCount", json!([])).await?)
    }

    pub async fn balance(&self, address: Address) -> Result<U256> {
        parse_hex_u256(
            &self
                .call(
                    "eth_getBalance",
                    json!([format!("{address:#x}"), "finalized"]),
                )
                .await?,
        )
    }

    pub async fn consensus_status(&self) -> Result<ConsensusStatus> {
        serde_json::from_value(self.call("outbe_consensusStatus", json!([])).await?)
            .wrap_err("invalid outbe_consensusStatus response")
    }

    pub async fn epoch_info(&self) -> Result<EpochInfo> {
        let value = self.call("outbe_getEpochInfo", json!([])).await?;
        Ok(EpochInfo {
            epoch_number: parse_hex_u64(value.get("epochNumber").unwrap_or(&Value::Null))
                .wrap_err("invalid outbe_getEpochInfo epoch number")?,
        })
    }

    pub async fn min_stake(&self) -> Result<U256> {
        let call = IStaking::getMinStakeCall {};
        let result = self.eth_call(STAKING, &call.abi_encode()).await?;
        Ok(IStaking::getMinStakeCall::abi_decode_returns(&result)?)
    }

    pub async fn finalized_tee_lease_policy(&self) -> Result<(Block, TeeLeasePolicy)> {
        let block = self.finalized_block().await?;
        let call = ITeeRegistryV1::activePolicyV1Call {};
        let result = self
            .eth_call_at(
                TEE_REGISTRY,
                &call.abi_encode(),
                &format!("0x{:x}", block.number),
            )
            .await?;
        let canonical = ITeeRegistryV1::activePolicyV1Call::abi_decode_returns(&result)?;
        Ok((block, decode_tee_lease_policy_v1(&canonical)?))
    }

    pub async fn validator(&self, address: Address) -> Result<ValidatorRecord> {
        let call = IValidatorSet::isValidatorCall { addr: address };
        let result = self.eth_call(VALIDATOR_SET, &call.abi_encode()).await?;
        if !IValidatorSet::isValidatorCall::abi_decode_returns(&result)? {
            return Ok(ValidatorRecord::default());
        }

        let call = IValidatorSet::validatorByAddressCall { addr: address };
        let result = self.eth_call(VALIDATOR_SET, &call.abi_encode()).await?;
        let record = IValidatorSet::validatorByAddressCall::abi_decode_returns(&result)?;
        Ok(ValidatorRecord {
            registered: !record.validatorAddress.is_zero(),
            status: record.status,
            has_bls_share: record.hasBLSShare,
        })
    }

    pub async fn transaction_block(&self, hash: &str) -> Result<Option<u64>> {
        let Some(receipt) = self
            .call_optional("eth_getTransactionReceipt", json!([hash]))
            .await?
        else {
            return Ok(None);
        };
        ensure!(
            receipt.get("status").and_then(Value::as_str) == Some("0x1"),
            "transaction reverted: {hash}"
        );
        receipt.get("blockNumber").map(parse_hex_u64).transpose()
    }

    async fn block(&self, tag: &str) -> Result<Block> {
        let value = self
            .call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        Ok(Block {
            number: parse_hex_u64(value.get("number").unwrap_or(&Value::Null))?,
            hash: value
                .get("hash")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre::eyre!("block response has no hash"))?
                .to_owned(),
            timestamp: parse_hex_u64(value.get("timestamp").unwrap_or(&Value::Null))?,
        })
    }

    async fn eth_call(&self, to: Address, data: &[u8]) -> Result<Vec<u8>> {
        self.eth_call_at(to, data, "finalized").await
    }

    async fn eth_call_at(&self, to: Address, data: &[u8], block: &str) -> Result<Vec<u8>> {
        let value = self
            .call(
                "eth_call",
                json!([{
                    "to": format!("{to:#x}"),
                    "data": format!("0x{}", hex::encode(data)),
                }, block]),
            )
            .await?;
        let encoded = value
            .as_str()
            .ok_or_else(|| eyre::eyre!("eth_call result is not hex"))?;
        let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
        hex::decode(encoded).wrap_err("invalid eth_call hex result")
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_optional(method, params)
            .await?
            .ok_or_else(|| eyre::eyre!("{method} returned null"))
    }

    async fn call_optional(&self, method: &str, params: Value) -> Result<Option<Value>> {
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        let response: Response = self
            .client
            .post(&self.url)
            .json(&Request {
                jsonrpc: "2.0",
                method,
                params,
                id,
            })
            .send()
            .await
            .wrap_err_with(|| format!("failed to contact {}", self.url))?
            .error_for_status()
            .wrap_err_with(|| format!("RPC HTTP error from {}", self.url))?
            .json()
            .await
            .wrap_err("invalid JSON-RPC response")?;
        ensure!(
            response.id.as_ref() == Some(&Value::from(id)),
            "JSON-RPC response id mismatch"
        );
        if let Some(error) = response.error {
            bail!(
                "JSON-RPC {method} failed ({}): {}",
                error.code,
                error.message
            );
        }
        Ok(response.result)
    }
}

fn decode_tee_lease_policy_v1(bytes: &[u8]) -> Result<TeeLeasePolicy> {
    ensure!(
        bytes.len() >= TEE_POLICY_V1_LEASE_PREFIX_LEN,
        "finalized TEE policy is truncated before its lease range"
    );
    ensure!(bytes[0] == 1, "unsupported finalized TEE policy version");
    let minimum = u64::from_be_bytes(
        bytes[TEE_POLICY_V1_MINIMUM_LEASE_OFFSET..TEE_POLICY_V1_MAXIMUM_LEASE_OFFSET]
            .try_into()
            .expect("fixed-size TEE policy field"),
    );
    let maximum = u64::from_be_bytes(
        bytes[TEE_POLICY_V1_MAXIMUM_LEASE_OFFSET..TEE_POLICY_V1_LEASE_PREFIX_LEN]
            .try_into()
            .expect("fixed-size TEE policy field"),
    );
    ensure!(
        minimum <= maximum,
        "finalized TEE policy has an invalid lease range {minimum}..={maximum} seconds"
    );
    Ok(TeeLeasePolicy { minimum, maximum })
}

fn parse_hex_u64(value: &Value) -> Result<u64> {
    let value = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("expected a hex quantity"))?;
    u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .wrap_err("invalid hex quantity")
}

fn parse_hex_u256(value: &Value) -> Result<U256> {
    let value = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("expected a hex quantity"))?;
    U256::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .map_err(|error| eyre::eyre!("invalid U256 quantity: {error}"))
}
