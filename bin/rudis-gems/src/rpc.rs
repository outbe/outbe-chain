use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use eyre::{ensure, Result, WrapErr};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub trait Rpc: Sync {
    fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value>> + Send;
}

pub struct Client {
    url: String,
    http: reqwest::Client,
    id: AtomicU64,
}
impl Client {
    pub fn new(url: String) -> Result<Self> {
        Ok(Self {
            url,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            id: AtomicU64::new(1),
        })
    }
}
impl Rpc for Client {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        let response: Value = self
            .http
            .post(&self.url)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .wrap_err("RPC connection failed")?
            .error_for_status()?
            .json()
            .await
            .wrap_err("Invalid JSON-RPC response")?;
        ensure!(response["id"] == id, "RPC response ID mismatch");
        if !response["error"].is_null() {
            eyre::bail!(
                "{method}: {}",
                response["error"]["message"]
                    .as_str()
                    .unwrap_or("RPC rejected request")
            );
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| eyre::eyre!("Missing RPC result"))
    }
}

pub fn quantity(value: &Value) -> Result<U256> {
    let text = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("Missing hex quantity"))?;
    U256::from_str_radix(
        text.strip_prefix("0x")
            .ok_or_else(|| eyre::eyre!("Invalid hex quantity"))?,
        16,
    )
    .map_err(|_| eyre::eyre!("Invalid uint256 quantity"))
}

pub async fn call<C: SolCall>(
    rpc: &impl Rpc,
    to: Address,
    call: C,
    block: &str,
) -> Result<C::Return> {
    let output = rpc
        .request(
            "eth_call",
            json!([{"to":to,"data":format!("0x{}",hex::encode(call.abi_encode()))},block]),
        )
        .await?;
    let bytes = hex::decode(
        output
            .as_str()
            .and_then(|s| s.strip_prefix("0x"))
            .ok_or_else(|| eyre::eyre!("Invalid eth_call result"))?,
    )?;
    Ok(C::abi_decode_returns_validate(&bytes)?)
}

pub async fn chain_id(rpc: &impl Rpc) -> Result<u64> {
    Ok(quantity(&rpc.request("eth_chainId", json!([])).await?)?.try_into()?)
}

pub async fn latest(rpc: &impl Rpc) -> Result<Value> {
    rpc.request("eth_getBlockByNumber", json!(["latest", false]))
        .await
}
