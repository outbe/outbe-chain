//! Read-only adapter for finalized TEE registry queries.

use alloy_primitives::Address;
use outbe_tee::registry_view::{ChainRpc, FinalizedStateRpc, RegistryRpc};

use crate::rpc::Rpc;

#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub(super) struct CliRpcReport(pub(super) eyre::Report);

pub(super) struct CliFinalityRpc<'a, R>(pub(super) &'a R);

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
