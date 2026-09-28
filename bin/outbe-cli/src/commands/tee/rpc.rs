use crate::abi;

use crate::rpc::Rpc;

use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;

use alloy_sol_types::SolValue;

use eyre::Result;
use eyre::WrapErr;

use outbe_operator::rpc::FinalityRpc;
use outbe_operator::rpc::RenewalRpc;

pub(super) struct CliFinalityRpc<'a, R>(pub(super) &'a R);

/// Read every context field from one finalized RPC block tag.
pub(super) async fn refresh_call_context(
    rpc: &(impl Rpc + Sync),
    height: Option<u64>,
) -> Result<()> {
    use outbe_tee::call_context::{EnclaveCallContextV1, EnclaveContextKindV1};
    let block = match height {
        Some(height) => rpc.eth_get_block_by_number(height).await?,
        None => rpc.eth_get_finalized_block().await?,
    };
    let number = json_hex_u64_field(&block, "number")?;
    let timestamp = json_hex_u64_field(&block, "timestamp")?;
    let tag = format!("0x{number:x}");
    let selector = keccak256(b"getActiveVersion()");
    let bytes = rpc
        .eth_call_at(
            outbe_primitives::addresses::UPDATE_ADDRESS,
            &selector[..4],
            &tag,
        )
        .await?;
    let version = U256::abi_decode(&bytes)?;
    eyre::ensure!(
        version <= U256::from(u32::MAX),
        "active protocol version overflow"
    );
    let genesis = rpc.eth_get_block_by_number(0).await?;
    let genesis_hash: B256 = genesis
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("genesis RPC block has no hash"))?
        .parse()?;
    outbe_tee::call_context::set_snapshot(EnclaveCallContextV1 {
        kind: EnclaveContextKindV1::Snapshot,
        chain_id: rpc.eth_chain_id().await?,
        genesis_hash,
        block_number: number,
        block_timestamp: timestamp,
        protocol_version: version.to(),
    })
    .map_err(eyre::Report::msg)
}

impl<R: Rpc + Sync> FinalityRpc for CliFinalityRpc<'_, R> {
    async fn transaction_receipt(
        &self,
        transaction_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        self.0.eth_get_transaction_receipt(transaction_hash).await
    }

    async fn logs(
        &self,
        address: Address,
        topics: &[Option<String>],
        from_block: &str,
        to_block: &str,
    ) -> Result<Vec<serde_json::Value>> {
        self.0
            .eth_get_logs(address, topics, from_block, to_block)
            .await
    }

    async fn block_by_number(&self, block: u64) -> Result<serde_json::Value> {
        self.0.eth_get_block_by_number(block).await
    }

    async fn finalized_block(&self) -> Result<serde_json::Value> {
        self.0.eth_get_finalized_block().await
    }

    async fn call_at(&self, to: Address, data: &[u8], block_tag: &str) -> Result<Vec<u8>> {
        self.0.eth_call_at(to, data, block_tag).await
    }
}

impl<R: Rpc + Sync> RenewalRpc for CliFinalityRpc<'_, R> {
    async fn chain_id(&self) -> Result<u64> {
        self.0.eth_chain_id().await
    }

    async fn gas_price(&self) -> Result<U256> {
        self.0.eth_gas_price().await
    }

    async fn transaction_count(&self, address: Address) -> Result<u64> {
        self.0.eth_get_transaction_count(address).await
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        self.0.eth_get_balance(address).await
    }

    async fn send_raw_transaction(&self, raw_transaction: &[u8]) -> Result<String> {
        self.0.eth_send_raw_transaction(raw_transaction).await
    }

    async fn tee_renewal_schedule_v1(
        &self,
    ) -> Result<outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1> {
        self.0.outbe_tee_renewal_schedule_v1().await
    }
}

pub(super) fn json_hex_bytes(value: &serde_json::Value, field: &str) -> Result<Vec<u8>> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("RPC response has no {field}"))?;
    hex::decode(raw.trim_start_matches("0x")).wrap_err_with(|| format!("decode RPC {field}"))
}

pub(super) fn json_hex_array(value: &serde_json::Value, field: &str) -> Result<Vec<Vec<u8>>> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("RPC response has no {field} array"))?
        .iter()
        .map(|item| {
            let raw = item
                .as_str()
                .ok_or_else(|| eyre::eyre!("RPC {field} contains a non-string node"))?;
            hex::decode(raw.trim_start_matches("0x"))
                .wrap_err_with(|| format!("decode RPC {field} node"))
        })
        .collect()
}

pub(super) fn json_hex_u64_field(value: &serde_json::Value, field: &str) -> Result<u64> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("RPC response has no {field}"))?;
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .wrap_err_with(|| format!("decode RPC {field}"))
}

pub(super) fn json_hex_u256_field(value: &serde_json::Value, field: &str) -> Result<U256> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("RPC response has no {field}"))?;
    U256::from_str_radix(raw.trim_start_matches("0x"), 16)
        .map_err(|error| eyre::eyre!("decode RPC {field}: {error}"))
}

pub(super) fn json_b256_field(value: &serde_json::Value, field: &str) -> Result<B256> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("RPC response has no {field}"))?;
    raw.parse::<B256>()
        .wrap_err_with(|| format!("decode RPC {field}"))
}

/// `eth_call` a view returning a single `uint256`.
pub(super) async fn call_u256(client: &(impl Rpc + Sync), call: Vec<u8>) -> Result<U256> {
    let result = client.eth_call(abi::TEE_REGISTRY_ADDR, &call).await?;
    U256::abi_decode(&result).wrap_err("decode uint256")
}
