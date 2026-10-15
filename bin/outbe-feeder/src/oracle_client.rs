//! Pinned oracle reads and explicit signing/broadcasting. A transport timeout is
//! an unknown transaction outcome, never permission to allocate a new nonce.
use std::time::Duration;

use alloy_eips::{eip1559::MIN_PROTOCOL_BASE_FEE, Encodable2718};
use alloy_network::{EthereumWallet, NetworkTransactionBuilder, TransactionBuilder};
use alloy_primitives::{keccak256, Address, Bytes};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use eyre::{Context, Result};
use serde_json::{json, Value};

use crate::{
    abi::{IOracle, IValidatorSet},
    config::{AccountConfig, ChainConfig},
    errors::OracleError,
    journal::PendingVote,
};

const ORACLE_ADDRESS: Address =
    alloy_primitives::address!("000000000000000000000000000000000000ee05");
const VALIDATOR_SET_ADDRESS: Address =
    alloy_primitives::address!("000000000000000000000000000000000000ee00");

#[derive(Clone, Debug)]
pub struct Head {
    pub height: u64,
    pub hash: String,
}
impl Head {
    pub fn period(&self, length: u64) -> u64 {
        self.height / length
    }
    fn block_id(&self) -> Value {
        json!({"blockHash":self.hash,"requireCanonical":true})
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreflightResult {
    Eligible,
    AlreadyVoted,
    Blocked(String),
}

#[derive(Debug)]
pub struct VoteReceipt {
    pub height: u64,
    pub hash: String,
    pub success: bool,
}

pub struct OracleClient {
    http: reqwest::Client,
    endpoint: String,
}
impl OracleClient {
    pub fn new(endpoint: &str) -> Result<Self> {
        reqwest::Url::parse(endpoint).context("invalid RPC URL")?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?,
            endpoint: endpoint.into(),
        })
    }
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let response: Value = self
            .http
            .post(&self.endpoint)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(error) = response.get("error") {
            return Err(OracleError::RpcFailure {
                method: method.into(),
                error: error.clone(),
            }
            .into());
        }
        response.get("result").cloned().ok_or_else(|| {
            OracleError::MissingResult {
                method: method.into(),
            }
            .into()
        })
    }
    pub async fn head(&self) -> Result<Head> {
        let value = self
            .rpc("eth_getBlockByNumber", json!(["latest", false]))
            .await?;
        Ok(Head {
            height: quantity(&value["number"])?,
            hash: value["hash"]
                .as_str()
                .ok_or(OracleError::MissingBlockHash)?
                .into(),
        })
    }
    pub async fn identity(
        &self,
        expected_chain: u64,
        signer: Address,
        validator: Address,
    ) -> Result<String> {
        let chain = quantity(&self.rpc("eth_chainId", json!([])).await?)?;
        if chain != expected_chain {
            return Err(OracleError::ChainIdMismatch {
                chain,
                expected_chain,
            }
            .into());
        }
        let genesis = self
            .rpc("eth_getBlockByNumber", json!(["0x0", false]))
            .await?;
        let hash = genesis["hash"]
            .as_str()
            .ok_or(OracleError::MissingGenesisHash)?;
        Ok(format!("{chain}:{hash}:{signer:#x}:{validator:#x}"))
    }
    async fn call<C: SolCall>(&self, head: &Head, to: Address, call: C) -> Result<C::Return> {
        let result = self
            .rpc(
                "eth_call",
                json!([{"to":to,"data":Bytes::from(call.abi_encode())},head.block_id()]),
            )
            .await?;
        let bytes: Bytes = serde_json::from_value(result)?;
        Ok(C::abi_decode_returns(&bytes)?)
    }
    pub async fn preflight(
        &self,
        head: &Head,
        period: u64,
        validator: Address,
        signer: Address,
    ) -> Result<PreflightResult> {
        let params = self
            .call(head, ORACLE_ADDRESS, IOracle::getParamsCall {})
            .await?;
        if !params.enabled {
            return Ok(PreflightResult::Blocked("oracle disabled".into()));
        }
        if params.votePeriod != period {
            return Ok(PreflightResult::Blocked(format!(
                "on-chain period {} != configured {period}",
                params.votePeriod
            )));
        }
        let status = self
            .call(
                head,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::validatorByAddressCall { addr: validator },
            )
            .await?;
        if status.status != 2 {
            return Ok(PreflightResult::Blocked(format!(
                "validator is not Active: {}",
                status.status
            )));
        }
        let resolved = self
            .call(
                head,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::resolveValidatorCall { role: 1, signer },
            )
            .await?;
        if resolved != validator {
            return Ok(PreflightResult::Blocked(
                "signer is not authorized for configured validator".into(),
            ));
        }
        let vote = self
            .call(
                head,
                ORACLE_ADDRESS,
                IOracle::getAggregateVoteCall { validator },
            )
            .await?;
        Ok(if vote.exists {
            PreflightResult::AlreadyVoted
        } else {
            PreflightResult::Eligible
        })
    }
    pub async fn price_observation(
        &self,
        head: &Head,
        base: Address,
        quote: Address,
    ) -> Result<(u64, u64)> {
        let data = self
            .call(
                head,
                ORACLE_ADDRESS,
                IOracle::getExchangeRateDataCall { base, quote },
            )
            .await?;
        Ok((data.lastBlock, data.lastTimestamp))
    }
    pub async fn nonce(&self, head: &Head, signer: Address) -> Result<u64> {
        quantity(
            &self
                .rpc("eth_getTransactionCount", json!([signer, head.block_id()]))
                .await?,
        )
    }
}

pub struct VoteSigningContext<'a> {
    pub wallet: &'a EthereumWallet,
    pub signer: Address,
    pub chain: &'a ChainConfig,
}

impl OracleClient {
    pub async fn sign_vote(
        &self,
        head: &Head,
        ctx: &VoteSigningContext<'_>,
        calldata: &[u8],
        replacement: Option<&PendingVote>,
    ) -> Result<PendingVote> {
        let nonce = self.nonce(head, ctx.signer).await?;
        // Do not jump over a transaction from another process using this key.
        let pending_nonce = quantity(
            &self
                .rpc("eth_getTransactionCount", json!([ctx.signer, "pending"]))
                .await?,
        )?;
        if let Some(old) = replacement {
            if nonce != old.nonce {
                return Err(OracleError::PendingNonceConsumed.into());
            }
            if pending_nonce > nonce.saturating_add(1) {
                return Err(OracleError::TransactionsQueuedBehindNonce.into());
            }
        } else if pending_nonce != nonce {
            return Err(OracleError::UntrackedPendingTransaction { nonce }.into());
        }
        let price = self.rpc("eth_gasPrice", json!([])).await?;
        let gas_price = u128::from_str_radix(
            price
                .as_str()
                .ok_or(OracleError::InvalidGasPrice)?
                .trim_start_matches("0x"),
            16,
        )?;
        let mut cap = gas_price
            .max(MIN_PROTOCOL_BASE_FEE as u128)
            .saturating_mul(2);
        if let Some(old) = replacement {
            cap = cap.max(
                old.max_fee_per_gas
                    .saturating_add(old.max_fee_per_gas / 4)
                    .saturating_add(1),
            );
        }
        let priority = if ctx.chain.gasless_oracle_votes {
            0
        } else {
            gas_price.max(replacement.map(|old| old.max_fee_per_gas).unwrap_or(0))
        };
        let mut tx = TransactionRequest::default()
            .from(ctx.signer)
            .to(ORACLE_ADDRESS)
            .input(Bytes::copy_from_slice(calldata).into())
            .gas_limit(1_000_000)
            .nonce(nonce)
            .max_fee_per_gas(cap)
            .max_priority_fee_per_gas(priority);
        tx.set_chain_id(ctx.chain.chain_id);
        let envelope = tx.build(ctx.wallet).await?;
        let raw = Bytes::from(envelope.encoded_2718());
        Ok(PendingVote {
            hash: format!("{:#x}", keccak256(&raw)),
            raw: format!("{raw:#x}"),
            nonce,
            observed_height: head.height,
            created_at: replacement.map(|old| old.created_at).unwrap_or_else(now),
            max_fee_per_gas: cap,
        })
    }
    pub async fn broadcast(&self, pending: &PendingVote) -> Result<()> {
        let bytes: Bytes = pending
            .raw
            .parse()
            .context("invalid pending raw transaction")?;
        if format!("{:#x}", keccak256(&bytes)) != pending.hash {
            return Err(OracleError::PendingHashMismatch.into());
        }
        let hash = match self
            .rpc("eth_sendRawTransaction", json!([pending.raw]))
            .await
        {
            Ok(hash) => hash,
            Err(error) => {
                // Reth can acknowledge an idempotent rebroadcast this way.
                // It is not inclusion: keep waiting for the receipt.
                let known = error.downcast_ref::<OracleError>().is_some_and(|rpc| {
                    matches!(
                        rpc,
                        OracleError::RpcFailure { method, error }
                            if method == "eth_sendRawTransaction"
                                && error["code"].as_i64() == Some(-32000)
                                && error["message"].as_str() == Some("already known")
                    )
                });
                if known {
                    return Ok(());
                }
                return Err(error);
            }
        };
        if hash.as_str() != Some(&pending.hash) {
            return Err(OracleError::UnexpectedBroadcastHash.into());
        }
        Ok(())
    }
    pub async fn receipt(&self, pending: &PendingVote) -> Result<Option<VoteReceipt>> {
        let r = self
            .rpc("eth_getTransactionReceipt", json!([pending.hash]))
            .await?;
        if r.is_null() {
            return Ok(None);
        }
        if r["transactionHash"].as_str() != Some(&pending.hash) {
            return Err(OracleError::ReceiptHashMismatch.into());
        }
        let height = quantity(&r["blockNumber"])?;
        let canonical = self
            .rpc(
                "eth_getBlockByNumber",
                json!([format!("{height:#x}"), false]),
            )
            .await?;
        if !canonical["hash"].is_string() || canonical["hash"] != r["blockHash"] {
            return Err(OracleError::ReceiptNotCanonical.into());
        }
        Ok(Some(VoteReceipt {
            height,
            hash: pending.hash.clone(),
            success: quantity(&r["status"])? == 1,
        }))
    }
}

pub fn create_wallet(account: &AccountConfig) -> Result<(EthereumWallet, Address)> {
    let signer: PrivateKeySigner = account.private_key.parse().context("invalid private key")?;
    let address = signer.address();
    Ok((EthereumWallet::from(signer), address))
}
fn quantity(value: &Value) -> Result<u64> {
    let text = value.as_str().ok_or(OracleError::InvalidQuantity)?;
    Ok(u64::from_str_radix(
        text.strip_prefix("0x")
            .ok_or(OracleError::InvalidQuantityPrefix)?,
        16,
    )?)
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_period_includes_zero_and_does_not_anticipate_boundary() {
        for (h, p) in [(0, 0), (6, 0), (7, 0), (8, 1), (15, 1), (16, 2)] {
            assert_eq!(
                Head {
                    height: h,
                    hash: String::new()
                }
                .period(8),
                p
            );
        }
    }
    #[test]
    fn rpc_quantity_rejects_bad_or_overflowing_data() {
        assert_eq!(quantity(&json!("0x123")).unwrap(), 291);
        for v in [
            json!(null),
            json!(12),
            json!("123"),
            json!("0x10000000000000000"),
        ] {
            assert!(quantity(&v).is_err());
        }
    }
}
