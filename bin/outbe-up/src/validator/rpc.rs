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
const HYPERLANE_CONTROLLER: Address = address!("000000000000000000000000000000000000EE14");

sol!("../../contracts/precompiles/src/IStaking.sol");
sol!("../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../contracts/precompiles/src/IHyperlaneController.sol");

pub struct RpcClient {
    url: String,
    client: reqwest::Client,
    id: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub number: u64,
    pub hash: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusStatus {
    #[serde(default)]
    pub connected_peers: u64,
    #[serde(default)]
    pub has_threshold_shares: bool,
    #[serde(default)]
    pub last_finalized_block: u64,
    #[serde(default)]
    pub next_planned_activation_height: u64,
    /// Absent when talking to a node version that predates maintenance guards.
    #[serde(default)]
    pub next_dkg_freeze_height: Option<u64>,
    /// Absent when talking to a node version that predates maintenance guards.
    #[serde(default)]
    pub maintenance_safe: Option<bool>,
    #[serde(default)]
    pub is_validator: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub enclave: EnclaveHealth,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnclaveHealth {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub offer_key_ready: bool,
}

impl EnclaveHealth {
    fn resident_ready(&self) -> bool {
        self.state == "ready" && self.ready && self.offer_key_ready
    }
}

impl ConsensusStatus {
    pub fn resident_ready(&self) -> bool {
        self.is_validator
            && matches!(
                self.mode.as_str(),
                "upstreamBootstrap" | "verifier" | "signer"
            )
            && self.enclave.resident_ready()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ValidatorRecord {
    pub registered: bool,
    pub status: u8,
    pub has_bls_share: bool,
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

    pub async fn min_stake(&self) -> Result<U256> {
        let call = IStaking::getMinStakeCall {};
        let result = self.eth_call(STAKING, &call.abi_encode()).await?;
        Ok(IStaking::getMinStakeCall::abi_decode_returns(&result)?)
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

    pub async fn validator_delegate(&self, validator: Address, role: u8) -> Result<Address> {
        let call = IValidatorSet::getDelegateCall { validator, role };
        let result = self.eth_call(VALIDATOR_SET, &call.abi_encode()).await?;
        Ok(IValidatorSet::getDelegateCall::abi_decode_returns(&result)?)
    }

    pub async fn hyperlane_signer(&self, validator: Address) -> Result<Address> {
        let call = IHyperlaneController::hyperlaneSignerCall { validator };
        let result = self
            .eth_call(HYPERLANE_CONTROLLER, &call.abi_encode())
            .await?;
        Ok(IHyperlaneController::hyperlaneSignerCall::abi_decode_returns(&result)?)
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
        .wrap_err("invalid U256 quantity")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_readiness_requires_live_key_and_validator_mode() {
        let status: ConsensusStatus = serde_json::from_value(json!({
            "isValidator": true, "mode": "upstreamBootstrap",
            "enclave": {"state": "ready", "ready": true, "offerKeyReady": true}
        }))
        .unwrap();
        assert!(status.resident_ready());
        let mut status = status;
        status.enclave.offer_key_ready = false;
        assert!(!status.resident_ready());
        status.enclave.offer_key_ready = true;
        status.is_validator = false;
        assert!(!status.resident_ready());
        let absent: ConsensusStatus = serde_json::from_value(json!({"isValidator": true})).unwrap();
        assert!(!absent.resident_ready());
    }

    #[test]
    fn old_consensus_status_omits_maintenance_fields() {
        let status: ConsensusStatus = serde_json::from_value(serde_json::json!({
            "lastFinalizedBlock": 42,
            "mode": "signer"
        }))
        .unwrap();
        assert_eq!(status.last_finalized_block, 42);
        assert_eq!(status.next_dkg_freeze_height, None);
        assert_eq!(status.maintenance_safe, None);
    }
}
