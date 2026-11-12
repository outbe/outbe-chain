//! Execution-layer capabilities consumed by the consensus node.
//!
//! These ports name only shared primitives and Reth interfaces, never the
//! concrete `outbe-execution` node or `outbe-evm` implementation.

use std::{future::Future, path::PathBuf, sync::Arc};

use alloy_primitives::B256;
use outbe_primitives::{
    projection::ProjectionCheckpoint, OutbeBlock, OutbeHeader, OutbePayloadTypes,
};
use reth_chainspec::ChainSpec;
use reth_ethereum::{
    node::api::{ConsensusEngineHandle, PayloadTypes},
    storage::{BlockIdReader, BlockReader, HeaderProvider, StateProviderFactory},
};
use reth_payload_builder::{PayloadBuilderError, PayloadBuilderHandle, PayloadKind};

pub use crate::follow::LocalBlockSource;

/// Engine handle client used by the executor and application handler.
pub trait EngineClient: Clone + Send + Sync + 'static {
    fn engine_handle(&self) -> ConsensusEngineHandle<OutbePayloadTypes>;
}

impl EngineClient for ConsensusEngineHandle<OutbePayloadTypes> {
    fn engine_handle(&self) -> ConsensusEngineHandle<OutbePayloadTypes> {
        self.clone()
    }
}

/// Payload service used by `OutbeApplication` to resolve proposed blocks.
pub trait PayloadBuilderPort: Clone + Send + Sync + 'static {
    fn payload_builder_handle(&self) -> PayloadBuilderHandle<OutbePayloadTypes>;

    fn resolve_kind(
        &self,
        id: alloy_rpc_types_engine::PayloadId,
        kind: PayloadKind,
    ) -> impl Future<
        Output = Option<
            Result<<OutbePayloadTypes as PayloadTypes>::BuiltPayload, PayloadBuilderError>,
        >,
    > + Send {
        let handle = self.payload_builder_handle();
        async move { handle.resolve_kind(id, kind).await }
    }
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
        + Clone
        + Send
        + Sync
        + 'static
{
}

/// Narrow provider seam implemented by the execution node bridge.
pub trait ExecutionProviderAccess: Clone + Send + Sync + 'static {
    type Provider: ExecutionStateAccess;

    fn provider(&self) -> &Self::Provider;
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
pub trait ExecutionNodeBackend: ExecutionProviderAccess + RecoveryForkchoiceReader {
    type EngineHandle: EngineClient;
    type PayloadBuilder: PayloadBuilderPort;
    type StateAccess: ExecutionStateAccess;
    type BlockSource: LocalBlockSource;

    fn chain_spec(&self) -> Arc<ChainSpec<OutbeHeader>>;
    fn beacon_engine_handle(&self) -> Self::EngineHandle;
    fn payload_builder_handle(&self) -> Self::PayloadBuilder;
    fn state_access(&self) -> &Self::StateAccess;
    fn block_source(&self) -> Self::BlockSource;
    fn data_dir(&self) -> PathBuf;
    fn p2p_secret_path(&self) -> PathBuf;
    fn has_p2p_secret_key_hex(&self) -> bool;
    fn load_p2p_secret_key(&self, path: PathBuf) -> eyre::Result<[u8; 32]>;
    fn collect_genesis_peer_evidence(&self) -> impl Future<Output = GenesisPeerEvidence> + Send;
}
