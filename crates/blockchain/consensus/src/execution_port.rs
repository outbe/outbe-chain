//! Execution-layer capabilities consumed by the consensus node.
//!
//! These ports name only shared primitives and Reth interfaces, never the
//! concrete `outbe-execution` node or `outbe-evm` implementation.

use std::{future::Future, path::PathBuf, sync::Arc};

use alloy_primitives::B256;
use outbe_primitives::{
    projection::ProjectionCheckpoint, OutbeBlock, OutbeHeader, OutbePayloadTypes, OutbePrimitives,
};
use reth_chain_state::CanonStateSubscriptions;
use reth_chainspec::ChainSpec;
use reth_node_builder::{ConsensusEngineHandle, FullNode, FullNodeComponents, NodeAddOns};
use reth_payload_builder::PayloadBuilderHandle;
use reth_provider::{BlockIdReader, BlockReader, HeaderProvider, StateProviderFactory};

pub use crate::follow::LocalBlockSource;

/// Payload service used by `OutbeApplication` to resolve proposed blocks.
pub trait PayloadBuilderPort: Clone + Send + Sync + 'static {
    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes>;
}

impl PayloadBuilderPort for PayloadBuilderHandle<OutbePayloadTypes> {
    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes> {
        self.clone()
    }
}

/// Provider reads needed by validator admission and the CL stack.
pub trait ExecutionStateAccess:
    StateProviderFactory
    + BlockReader<Block = OutbeBlock>
    + BlockIdReader
    + HeaderProvider<Header = OutbeHeader>
    + CanonStateSubscriptions<Primitives = OutbePrimitives>
    + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> ExecutionStateAccess for T where
    T: StateProviderFactory
        + BlockReader<Block = OutbeBlock>
        + BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + CanonStateSubscriptions<Primitives = OutbePrimitives>
        + Clone
        + Send
        + Sync
        + 'static
{
}

/// Narrow provider seam also implemented by existing launched Reth nodes.
pub trait ExecutionProviderAccess: Clone + Send + Sync + 'static {
    type Provider: ExecutionStateAccess;

    fn provider(&self) -> &Self::Provider;
}

impl<Node, AddOns> ExecutionProviderAccess for FullNode<Node, AddOns>
where
    Node: FullNodeComponents<Provider: ExecutionStateAccess>,
    AddOns: NodeAddOns<Node>,
    Self: Send + Sync + 'static,
{
    type Provider = Node::Provider;

    fn provider(&self) -> &Self::Provider {
        &self.provider
    }
}

/// Canonical in-memory forkchoice, not persisted execution progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryForkchoice {
    pub head: ProjectionCheckpoint,
    pub safe: Option<ProjectionCheckpoint>,
    pub finalized: Option<ProjectionCheckpoint>,
}

/// Reads the EL's current recovery identity without exposing its database.
pub trait RecoveryForkchoiceReader {
    fn recovery_forkchoice(&self) -> Result<RecoveryForkchoice, crate::errors::StackRecoveryError>;
}

/// One connected peer's execution-chain identity at genesis formation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisPeerStatus {
    pub genesis: B256,
    pub blockhash: B256,
    pub latest_block: Option<u64>,
}

/// Network evidence consumed by the CL genesis-formation gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenesisPeerEvidence {
    pub connected_peers: usize,
    pub is_syncing: bool,
    pub is_initially_syncing: bool,
    pub peer_query_failed: bool,
    pub peers: Vec<GenesisPeerStatus>,
}

/// Launched execution-node services required when assembling the CL stack.
///
/// The engine bridge implements this port; the provider-only compatibility
/// implementation above does not opt arbitrary Reth nodes into this contract.
pub trait ExecutionNodeBackend: ExecutionProviderAccess {
    fn chain_spec(&self) -> &Arc<ChainSpec<OutbeHeader>>;
    fn beacon_engine_handle(&self) -> ConsensusEngineHandle<OutbePayloadTypes>;
    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes>;
    fn data_dir(&self) -> PathBuf;
    fn p2p_secret_path(&self) -> PathBuf;
    fn has_p2p_secret_key_hex(&self) -> bool;
    fn collect_genesis_peer_evidence(&self) -> impl Future<Output = GenesisPeerEvidence> + Send;
}
