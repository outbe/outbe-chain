use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct Rpc {
    client: reqwest::Client,
    url: String,
}
impl Rpc {
    pub fn new(port: u16) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            url: format!("http://127.0.0.1:{port}"),
        }
    }
    pub async fn request(&self, method: &str, params: Value) -> Option<Value> {
        let value: Value = self
            .client
            .post(&self.url)
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method,"params":params}))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        value.get("result").cloned()
    }
    pub async fn finalized(&self) -> Option<u64> {
        let block = self
            .request("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        u64::from_str_radix(block["number"].as_str()?.trim_start_matches("0x"), 16).ok()
    }
    pub async fn account(&self, method: &str, address: Address) -> U256 {
        let result = self
            .request(method, json!([address, "latest"]))
            .await
            .unwrap();
        U256::from_str_radix(result.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }
    pub async fn call<C: SolCall>(&self, address: Address, call: C) -> C::Return {
        let result = self.request("eth_call", json!([{"to":address, "data":format!("0x{}", hex::encode(call.abi_encode()))}, "latest"]))
            .await.unwrap();
        C::abi_decode_returns(
            &hex::decode(result.as_str().unwrap().trim_start_matches("0x")).unwrap(),
        )
        .unwrap()
    }
}
