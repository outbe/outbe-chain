use std::future::Future;

use crate::abi;
use crate::errors::TeeRpcError;
use crate::rpc::Rpc;

use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;

use alloy_sol_types::SolValue;

use eyre::Result;
use eyre::WrapErr;

pub(super) use outbe_tee::registry_view::{ChainRpc, FinalizedStateRpc, RegistryRpc};

#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub(super) struct CliRpcReport(pub(super) eyre::Report);

pub(super) trait TransactionReceiptRpc {
    fn transaction_receipt(
        &self,
        transaction_hash: &str,
    ) -> impl Future<Output = Result<Option<serde_json::Value>>> + Send;
}

pub(super) trait FinalityRpc: FinalizedStateRpc + TransactionReceiptRpc {
    fn logs(
        &self,
        address: Address,
        topics: &[Option<String>],
        from_block: &str,
        to_block: &str,
    ) -> impl Future<Output = Result<Vec<serde_json::Value>>> + Send;

    fn block_by_number(&self, block: u64)
        -> impl Future<Output = Result<serde_json::Value>> + Send;
}

pub(super) trait RelayPreparationRpc: ChainRpc {
    fn gas_price(&self) -> impl Future<Output = Result<U256>> + Send;

    fn transaction_count(&self, address: Address) -> impl Future<Output = Result<u64>> + Send;

    fn balance(&self, address: Address) -> impl Future<Output = Result<U256>> + Send;
}

pub(super) trait RelayRpc: TransactionReceiptRpc {
    fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
    ) -> impl Future<Output = Result<String>> + Send;
}

pub(super) trait RenewalRpc: RegistryRpc + RelayPreparationRpc + RelayRpc {}

impl<T> RenewalRpc for T where T: RegistryRpc + RelayPreparationRpc + RelayRpc {}

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
    if version > U256::from(u32::MAX) {
        return Err(TeeRpcError::ProtocolVersionOverflow.into());
    }
    let genesis = rpc.eth_get_block_by_number(0).await?;
    let genesis_hash: B256 = genesis
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .ok_or(TeeRpcError::MissingGenesisHash)?
        .parse()?;
    outbe_tee::call_context::set_snapshot(EnclaveCallContextV1 {
        kind: EnclaveContextKindV1::Snapshot,
        chain_id: rpc.eth_chain_id().await?,
        genesis_hash,
        block_number: number,
        block_timestamp: timestamp,
        protocol_version: version.to(),
    })
    .map_err(Into::into)
}

impl<R: Rpc + Sync> TransactionReceiptRpc for CliFinalityRpc<'_, R> {
    async fn transaction_receipt(
        &self,
        transaction_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        self.0.eth_get_transaction_receipt(transaction_hash).await
    }
}

impl<R: Rpc + Sync> FinalityRpc for CliFinalityRpc<'_, R> {
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
}

impl<R: Rpc + Sync> ChainRpc for CliFinalityRpc<'_, R> {
    type Error = CliRpcReport;

    async fn chain_id(&self) -> std::result::Result<u64, Self::Error> {
        self.0.eth_chain_id().await.map_err(CliRpcReport)
    }
}

impl<R: Rpc + Sync> FinalizedStateRpc for CliFinalityRpc<'_, R> {
    async fn finalized_block(&self) -> std::result::Result<serde_json::Value, Self::Error> {
        self.0.eth_get_finalized_block().await.map_err(CliRpcReport)
    }

    async fn call_at(
        &self,
        to: Address,
        data: &[u8],
        block_tag: &str,
    ) -> std::result::Result<Vec<u8>, Self::Error> {
        self.0
            .eth_call_at(to, data, block_tag)
            .await
            .map_err(CliRpcReport)
    }
}

impl<R: Rpc + Sync> RelayPreparationRpc for CliFinalityRpc<'_, R> {
    async fn gas_price(&self) -> Result<U256> {
        self.0.eth_gas_price().await
    }

    async fn transaction_count(&self, address: Address) -> Result<u64> {
        self.0.eth_get_transaction_count(address).await
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        self.0.eth_get_balance(address).await
    }
}

impl<R: Rpc + Sync> RelayRpc for CliFinalityRpc<'_, R> {
    async fn send_raw_transaction(&self, raw_transaction: &[u8]) -> Result<String> {
        self.0.eth_send_raw_transaction(raw_transaction).await
    }
}

impl<R: Rpc + Sync> RegistryRpc for CliFinalityRpc<'_, R> {
    async fn tee_renewal_schedule_v1(
        &self,
    ) -> std::result::Result<outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1, Self::Error>
    {
        self.0
            .outbe_tee_renewal_schedule_v1()
            .await
            .map_err(CliRpcReport)
    }
}

pub(super) fn json_hex_bytes(value: &serde_json::Value, field: &str) -> Result<Vec<u8>> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| TeeRpcError::MissingField(field.to_owned()))?;
    hex::decode(raw.trim_start_matches("0x")).wrap_err_with(|| format!("decode RPC {field}"))
}

pub(super) fn json_hex_array(value: &serde_json::Value, field: &str) -> Result<Vec<Vec<u8>>> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| TeeRpcError::MissingArrayField(field.to_owned()))?
        .iter()
        .map(|item| {
            let raw = item
                .as_str()
                .ok_or_else(|| TeeRpcError::NonStringArrayNode(field.to_owned()))?;
            hex::decode(raw.trim_start_matches("0x"))
                .wrap_err_with(|| format!("decode RPC {field} node"))
        })
        .collect()
}

pub(super) fn json_hex_u64_field(value: &serde_json::Value, field: &str) -> Result<u64> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| TeeRpcError::MissingField(field.to_owned()))?;
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .wrap_err_with(|| format!("decode RPC {field}"))
}

pub(super) fn json_hex_u256_field(value: &serde_json::Value, field: &str) -> Result<U256> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| TeeRpcError::MissingField(field.to_owned()))?;
    U256::from_str_radix(raw.trim_start_matches("0x"), 16).map_err(|source| {
        TeeRpcError::DecodeU256Field {
            field: field.to_owned(),
            source,
        }
        .into()
    })
}

pub(super) fn json_b256_field(value: &serde_json::Value, field: &str) -> Result<B256> {
    let raw = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| TeeRpcError::MissingField(field.to_owned()))?;
    raw.parse::<B256>()
        .wrap_err_with(|| format!("decode RPC {field}"))
}

/// `eth_call` a view returning a single `uint256`.
pub(super) async fn call_u256(client: &(impl Rpc + Sync), call: Vec<u8>) -> Result<U256> {
    let result = client.eth_call(abi::TEE_REGISTRY_ADDR, &call).await?;
    U256::abi_decode(&result).wrap_err("decode uint256")
}
