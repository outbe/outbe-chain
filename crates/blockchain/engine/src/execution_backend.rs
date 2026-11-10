//! Concrete execution node adapter for the consensus-owned CL ports.

use std::{future::Future, path::PathBuf, sync::Arc};

use commonware_runtime::{BufferPooler, Clock, Metrics, Network, Resolver, Spawner, Storage};
use outbe_consensus::{
    execution_port::{
        ExecutionNodeBackend, ExecutionProviderAccess, GenesisPeerEvidence, RecoveryForkchoice,
        RecoveryForkchoiceReader,
    },
    follow::LocalBlockSource,
    stack::StackNodeAccess,
};
use outbe_execution::OutbeFullNode;
use outbe_primitives::{OutbeHeader, OutbePayloadTypes};
use rand_core_commonware::CryptoRng;
use reth_ethereum::chainspec::ChainSpec;
use reth_node_builder::ConsensusEngineHandle;
use reth_payload_builder::PayloadBuilderHandle;

/// Launched Reth node connected to the consensus execution ports.
#[derive(Clone)]
pub struct RethExecutionBackend(pub OutbeFullNode);

impl RethExecutionBackend {
    pub fn new(node: OutbeFullNode) -> Self {
        Self(node)
    }

    pub fn inner(&self) -> &OutbeFullNode {
        &self.0
    }

    pub fn into_inner(self) -> OutbeFullNode {
        self.0
    }
}

impl From<OutbeFullNode> for RethExecutionBackend {
    fn from(node: OutbeFullNode) -> Self {
        Self::new(node)
    }
}

impl ExecutionProviderAccess for RethExecutionBackend {
    type Provider = <OutbeFullNode as ExecutionProviderAccess>::Provider;

    fn provider(&self) -> &Self::Provider {
        self.0.provider()
    }
}

impl RecoveryForkchoiceReader for RethExecutionBackend {
    fn recovery_forkchoice(
        &self,
    ) -> Result<RecoveryForkchoice, outbe_consensus::errors::StackRecoveryError> {
        self.0.recovery_forkchoice()
    }
}

impl ExecutionNodeBackend for RethExecutionBackend {
    fn chain_spec(&self) -> &Arc<ChainSpec<OutbeHeader>> {
        &self.0.config.chain
    }

    fn beacon_engine_handle(&self) -> ConsensusEngineHandle<OutbePayloadTypes> {
        StackNodeAccess::beacon_engine_handle(&self.0)
    }

    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes> {
        StackNodeAccess::payload_builder_handle(&self.0)
    }

    fn data_dir(&self) -> PathBuf {
        StackNodeAccess::data_dir(&self.0)
    }

    fn p2p_secret_path(&self) -> PathBuf {
        StackNodeAccess::p2p_secret_path(&self.0)
    }

    fn has_p2p_secret_key_hex(&self) -> bool {
        StackNodeAccess::has_p2p_secret_key_hex(&self.0)
    }

    fn collect_genesis_peer_evidence(&self) -> impl Future<Output = GenesisPeerEvidence> + Send {
        StackNodeAccess::collect_genesis_peer_evidence(&self.0)
    }
}

impl StackNodeAccess for RethExecutionBackend {
    fn chain_spec(&self) -> Arc<ChainSpec<OutbeHeader>> {
        Arc::clone(ExecutionNodeBackend::chain_spec(self))
    }

    fn beacon_engine_handle(&self) -> ConsensusEngineHandle<OutbePayloadTypes> {
        ExecutionNodeBackend::beacon_engine_handle(self)
    }

    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes> {
        ExecutionNodeBackend::payload_builder_handle(self)
    }

    fn data_dir(&self) -> PathBuf {
        ExecutionNodeBackend::data_dir(self)
    }

    fn p2p_secret_path(&self) -> PathBuf {
        ExecutionNodeBackend::p2p_secret_path(self)
    }

    fn has_p2p_secret_key_hex(&self) -> bool {
        ExecutionNodeBackend::has_p2p_secret_key_hex(self)
    }

    fn load_p2p_secret_key(&self, path: PathBuf) -> eyre::Result<[u8; 32]> {
        StackNodeAccess::load_p2p_secret_key(&self.0, path)
    }

    fn collect_genesis_peer_evidence(&self) -> impl Future<Output = GenesisPeerEvidence> + Send {
        ExecutionNodeBackend::collect_genesis_peer_evidence(self)
    }
}

impl LocalBlockSource for RethExecutionBackend {
    fn get_block_by_digest(
        &self,
        digest: outbe_consensus::digest::Digest,
    ) -> impl Future<Output = Option<outbe_consensus::block::ConsensusBlock>> + Send {
        let source = outbe_consensus::follow_transport::RethLocalBlockSource::new(self.clone());
        async move { source.get_block_by_digest(digest).await }
    }
}

/// Run the consensus stack over a launched execution node or its bridge adapter.
pub fn run_consensus_with_execution<E>(
    ctx: E,
    args: outbe_consensus::args::ConsensusArgs,
    node: impl Into<RethExecutionBackend>,
    bridge: outbe_primitives::consensus::ConsensusExecutionBridge,
    services: outbe_consensus::stack::ConsensusStackServices,
) -> impl Future<Output = eyre::Result<()>> + Send
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    outbe_consensus::stack::run_consensus_stack(ctx, args, node.into(), bridge, services)
}
