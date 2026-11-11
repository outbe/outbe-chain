//! Concrete execution node adapter for the consensus-owned CL ports.

use std::{future::Future, path::PathBuf, sync::Arc};

use alloy_primitives::B256;
use commonware_runtime::{BufferPooler, Clock, Metrics, Network, Resolver, Spawner, Storage};
use outbe_consensus::{
    block::ConsensusBlock,
    execution_port::{
        ExecutionNodeBackend, ExecutionProviderAccess, GenesisPeerEvidence, GenesisPeerStatus,
        RecoveryForkchoice, RecoveryForkchoiceReader,
    },
    follow::LocalBlockSource,
    stack::recovery::forkchoice::RecoveredRethForkchoice,
};
use outbe_execution::OutbeFullNode;
use outbe_primitives::{
    projection::ProjectionCheckpoint, OutbeHeader, OutbePayloadTypes, OutbePrimitives,
};
use rand_core_commonware::CryptoRng;
use reth_ethereum::{
    chainspec::{ChainSpec, EthChainSpec as _},
    network::api::{NetworkInfo, Peers, PeersInfo},
    node::api::NodeTypesWithDBAdapter,
    provider::db::DatabaseEnv,
    storage::{BlockReader, TransactionVariant},
};
use reth_node_builder::ConsensusEngineHandle;
use reth_payload_builder::PayloadBuilderHandle;
use reth_provider::providers::BlockchainProvider;
use tracing::{debug, warn};

/// Concrete Reth provider state-access implementation for the launched node.
pub type RethStateAccess =
    BlockchainProvider<NodeTypesWithDBAdapter<outbe_execution::OutbeNode, DatabaseEnv>>;

/// Reads already-imported blocks from the local Reth execution DB by hash.
///
/// The follower imports finalized blocks through the executor (FCU + newPayload).
/// Thus, by the time the marshal asks to backfill a `Request::Block(digest)`, the
/// block is in the EL DB. This is the same lookup the validator path's resolver
/// performs against peers, but sourced locally.
#[derive(Clone)]
pub struct RethLocalBlockSource<N> {
    node: N,
}

impl<N: ExecutionProviderAccess> RethLocalBlockSource<N> {
    pub fn new(node: N) -> Self {
        Self { node }
    }
}

impl<N: ExecutionProviderAccess> LocalBlockSource for RethLocalBlockSource<N> {
    fn get_block_by_digest(
        &self,
        digest: outbe_consensus::digest::Digest,
    ) -> impl Future<Output = Option<ConsensusBlock>> + Send {
        let node = self.node.clone();
        async move {
            let hash: B256 = digest.0;
            match node
                .provider()
                .recovered_block(hash.into(), TransactionVariant::NoHash)
            {
                Ok(Some(recovered)) => {
                    Some(ConsensusBlock::from_sealed(recovered.into_sealed_block()))
                }
                Ok(None) => {
                    debug!(%hash, "local EL has no block for requested digest");
                    None
                }
                Err(error) => {
                    debug!(%hash, %error, "failed reading block from local EL");
                    None
                }
            }
        }
    }
}

fn read_reth_recovery_forkchoice_raw(
    state: &reth_chain_state::CanonicalInMemoryState<OutbePrimitives>,
) -> Result<RecoveryForkchoice, outbe_consensus::errors::StackRecoveryError> {
    // Persisted execution progress may remain ahead after a canonical rollback.
    let head = state.get_canonical_head();
    if head.hash() == B256::ZERO {
        return Err(outbe_consensus::errors::StackRecoveryError::RethZeroCanonicalHead);
    }
    let checkpoint = |checkpoint: alloy_eips::BlockNumHash| ProjectionCheckpoint {
        block_number: checkpoint.number,
        block_hash: checkpoint.hash,
    };
    Ok(RecoveryForkchoice {
        head: checkpoint(head.num_hash()),
        safe: state.get_safe_num_hash().map(checkpoint),
        finalized: state.get_finalized_num_hash().map(checkpoint),
    })
}

/// Reads the canonical in-memory Reth forkchoice state for startup recovery.
pub fn read_reth_recovery_forkchoice(
    state: &reth_chain_state::CanonicalInMemoryState<OutbePrimitives>,
) -> eyre::Result<RecoveredRethForkchoice> {
    Ok(read_reth_recovery_forkchoice_raw(state)?.into())
}

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

    /// Creates the Reth-backed finalization barrier for this execution node.
    pub fn finalized_execution_barrier(
        &self,
    ) -> Arc<dyn outbe_consensus::executor::actor::FinalizedExecutionBarrier> {
        Arc::new(crate::finalization::RethFinalizationBarrier::new(Arc::new(
            crate::finalization::RethDurableExecutionState::new(self.0.provider.clone()),
        )))
    }

    /// Bundles the execution-side consensus services with this node's finalization barrier.
    pub fn execution_services(
        &self,
        finalization_notify: Arc<tokio::sync::Notify>,
        tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    ) -> outbe_consensus::stack::ConsensusExecutionServices {
        outbe_consensus::stack::ConsensusExecutionServices {
            finalized_execution_barrier: self.finalized_execution_barrier(),
            finalization_notify,
            tee_lease_armed_rx,
        }
    }
}

impl From<OutbeFullNode> for RethExecutionBackend {
    fn from(node: OutbeFullNode) -> Self {
        Self::new(node)
    }
}

impl ExecutionProviderAccess for RethExecutionBackend {
    type Provider = RethStateAccess;

    fn provider(&self) -> &Self::Provider {
        &self.0.provider
    }
}

impl RecoveryForkchoiceReader for RethExecutionBackend {
    fn recovery_forkchoice(
        &self,
    ) -> Result<RecoveryForkchoice, outbe_consensus::errors::StackRecoveryError> {
        read_reth_recovery_forkchoice_raw(&self.0.provider.canonical_in_memory_state())
    }
}

impl ExecutionNodeBackend for RethExecutionBackend {
    type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;
    type PayloadBuilder = PayloadBuilderHandle<OutbePayloadTypes>;
    type StateAccess = RethStateAccess;
    type BlockSource = RethLocalBlockSource<Self>;

    fn chain_spec(&self) -> Arc<ChainSpec<OutbeHeader>> {
        self.0.chain_spec()
    }

    fn beacon_engine_handle(&self) -> Self::EngineHandle {
        self.0.add_ons_handle.beacon_engine_handle.clone()
    }

    fn payload_builder_handle(&self) -> Self::PayloadBuilder {
        self.0.payload_builder_handle.clone()
    }

    fn state_access(&self) -> &Self::StateAccess {
        &self.0.provider
    }

    fn block_source(&self) -> Self::BlockSource {
        RethLocalBlockSource::new(self.clone())
    }

    fn data_dir(&self) -> PathBuf {
        self.0
            .config
            .datadir
            .clone()
            .resolve_datadir(self.0.chain_spec().chain())
            .data_dir()
            .to_path_buf()
    }

    fn p2p_secret_path(&self) -> PathBuf {
        self.0.data_dir.p2p_secret()
    }

    fn has_p2p_secret_key_hex(&self) -> bool {
        self.0.config.network.p2p_secret_key_hex.is_some()
    }

    fn load_p2p_secret_key(&self, path: PathBuf) -> eyre::Result<[u8; 32]> {
        Ok(self.0.config.network.secret_key(path)?.secret_bytes())
    }

    async fn collect_genesis_peer_evidence(&self) -> GenesisPeerEvidence {
        let peers_result = self.0.network.get_all_peers().await;
        let (peer_query_failed, peers) = match peers_result {
            Ok(peers) => (
                false,
                peers
                    .into_iter()
                    .map(|peer| GenesisPeerStatus {
                        genesis: peer.status.genesis,
                        blockhash: peer.status.blockhash,
                        latest_block: peer.status.latest_block,
                    })
                    .collect(),
            ),
            Err(error) => {
                warn!(
                    ?error,
                    "failed to query Reth peers during genesis formation gate"
                );
                (true, Vec::new())
            }
        };
        GenesisPeerEvidence {
            connected_peers: self.0.network.num_connected_peers(),
            is_syncing: self.0.network.is_syncing(),
            is_initially_syncing: self.0.network.is_initially_syncing(),
            peer_query_failed,
            peers,
        }
    }
}

impl LocalBlockSource for RethExecutionBackend {
    fn get_block_by_digest(
        &self,
        digest: outbe_consensus::digest::Digest,
    ) -> impl Future<Output = Option<ConsensusBlock>> + Send {
        let source = self.block_source();
        async move { source.get_block_by_digest(digest).await }
    }
}

/// Run the consensus stack over a launched execution node or its bridge adapter.
pub fn run_consensus_with_execution<E>(
    ctx: E,
    args: outbe_consensus::args::ConsensusArgs,
    node: impl Into<RethExecutionBackend>,
    bridge: outbe_primitives::consensus::ConsensusExecutionBridge,
    mut services: outbe_consensus::stack::ConsensusStackServices,
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
    let backend = node.into();
    services.finalized_execution_barrier = backend.finalized_execution_barrier();
    outbe_consensus::stack::run_consensus_stack(ctx, args, backend, bridge, services)
}
