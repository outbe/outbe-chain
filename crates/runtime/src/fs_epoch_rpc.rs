//! `fsEpoch_*` JSON-RPC namespace serving the native `FsEpochRegistry` precompile's head
//! epoch (`fsEpoch_head`), matching the wire format expected by `bank-data-provider`.

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use outbe_fsepochregistry::FsEpochRegistryContract;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::{
    readonly::{ReadOnlyStorageProvider, StorageReader},
    StorageHandle,
};
use reth_ethereum::storage::{StateProvider as _, StateProviderBox};
use reth_provider::{BlockIdReader, StateProviderFactory};
use serde::{Deserialize, Serialize};

pub(crate) const FS_EPOCH_RPC_NAMESPACE: &str = "fsEpoch";

/// Error code for "the chain has no finalized block yet" (matches `pso-chain-next`).
pub const ERR_NOT_READY: i32 = -32000;

/// Error code for a state provider or storage read failure.
pub const ERR_PROVIDER: i32 = -32002;

/// JSON-RPC payload returned by `fsEpoch_head`.
///
/// ```json
/// { "anchored": true, "head": 3, "startWwd": 20261001, "digest": "0x..." }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEpochHeadResponse {
    pub anchored: bool,
    pub head: u32,
    pub start_wwd: u32,
    pub digest: B256,
}

impl From<outbe_fsepochregistry::FsEpochHead> for FsEpochHeadResponse {
    fn from(head: outbe_fsepochregistry::FsEpochHead) -> Self {
        Self {
            anchored: head.anchored,
            head: head.head,
            start_wwd: head.start_wwd,
            digest: head.digest,
        }
    }
}

/// The `fsEpoch_*` JSON-RPC API.
#[rpc(server, namespace = "fsEpoch")]
pub trait FsEpochApi {
    /// Reads the `FsEpochRegistry` head from finalized EL state (`fsEpoch_head`).
    #[method(name = "head", blocking)]
    fn head(&self) -> RpcResult<FsEpochHeadResponse>;
}

struct RethStateReader<'a> {
    state: &'a StateProviderBox,
}

impl StorageReader for RethStateReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|opt| opt.unwrap_or(U256::ZERO))
            .map_err(|e| PrecompileError::Storage(e.to_string()))
    }
}

/// Handler serving `fsEpoch_head` directly from the native `FsEpochRegistry` precompile storage.
#[derive(Clone)]
pub struct FsEpochRpcHandler<P> {
    provider: Arc<P>,
}

impl<P> FsEpochRpcHandler<P> {
    #[must_use]
    pub fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

fn rpc_err(code: i32, msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(code, msg.into(), None::<()>)
}

impl<P> FsEpochApiServer for FsEpochRpcHandler<P>
where
    P: BlockIdReader + StateProviderFactory + Send + Sync + 'static,
{
    fn head(&self) -> RpcResult<FsEpochHeadResponse> {
        let finalized = self
            .provider
            .finalized_block_num_hash()
            .map_err(|e| rpc_err(ERR_PROVIDER, format!("finalized_block_num_hash: {e}")))?
            .ok_or_else(|| rpc_err(ERR_NOT_READY, "no finalized block yet; retry shortly"))?;
        let state = self
            .provider
            .state_by_block_hash(finalized.hash)
            .map_err(|e| rpc_err(ERR_PROVIDER, format!("state_by_block_hash: {e}")))?;
        let reader = RethStateReader { state: &state };
        let mut ro = ReadOnlyStorageProvider::new(reader);
        let head = StorageHandle::enter(&mut ro, |storage| {
            FsEpochRegistryContract::new(storage).head_info()
        })
        .map_err(|e| rpc_err(ERR_PROVIDER, format!("reading FsEpochRegistry head: {e}")))?;
        Ok(head.into())
    }
}

pub(crate) fn register_fs_epoch_rpc_if_configured(
    modules: &mut reth_ethereum::rpc::builder::TransportRpcModules,
    fs_epoch: impl Into<jsonrpsee::Methods>,
) -> Result<(), jsonrpsee::core::RegisterMethodError> {
    modules.merge_if_module_configured(
        crate::RethRpcModule::Other(FS_EPOCH_RPC_NAMESPACE.to_owned()),
        fs_epoch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::OutbeHeader;
    use reth_provider::test_utils::NoopProvider;

    #[test]
    fn registers_under_fs_epoch_head_method_name() {
        let provider: Arc<
            NoopProvider<reth_chainspec::ChainSpec<OutbeHeader>, outbe_primitives::OutbePrimitives>,
        > = Arc::new(NoopProvider::new(Arc::new(
            reth_chainspec::ChainSpec::default(),
        )));
        let module = FsEpochRpcHandler::new(provider).into_rpc();
        let names: Vec<&str> = module.method_names().collect();
        assert_eq!(names, ["fsEpoch_head"]);
    }

    #[test]
    fn head_response_serializes_in_bdp_camel_case_shape() {
        let resp = FsEpochHeadResponse {
            anchored: true,
            head: 3,
            start_wwd: 20_261_001,
            digest: B256::repeat_byte(0xAB),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["anchored"], true);
        assert_eq!(json["head"], 3);
        assert_eq!(json["startWwd"], 20_261_001);
        assert_eq!(json["digest"], format!("{:#x}", B256::repeat_byte(0xAB)));
    }
}
