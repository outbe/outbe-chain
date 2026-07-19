//! Outbe node type definition.
//!
//! Defines `OutbeNode` which uses standard Ethereum primitives but customizes
//! the executor (stateful precompiles) and consensus (increased extra_data).

use crate::{
    consensus::OutbeConsensusBuilder, engine::OutbeEngineValidatorBuilder,
    payload_builder::OutbePayloadBuilder,
};
use outbe_compressed_entities::CompressedTreeService;
use outbe_evm::{OutbeExecutorBuilder, SharedOutbeEvmSigner};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::{
    consensus::ConsensusExecutionBridge, OutbeHeader, OutbePayloadTypes, OutbePrimitives,
    OutbeTxEnvelope,
};
use outbe_txpool::OutbePoolBuilder;
use reth_chainspec::{ChainSpec, EthChainSpec};
use reth_ethereum::node::node::EthereumEthApiBuilder;
use reth_ethereum::node::EthereumNetworkBuilder;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_node_builder::{
    components::{BasicPayloadServiceBuilder, ComponentsBuilder, PayloadBuilderBuilder},
    node::{FullNodeTypes, NodeTypes},
    rpc::{BasicEngineValidatorBuilder, NoopEngineApiBuilder, RpcAddOns},
    BuilderContext, Node, NodeAdapter, PayloadBuilderConfig,
};
use reth_provider::EthStorage;
use reth_transaction_pool::{PoolTransaction, TransactionPool};

/// Outbe node type configuration.
///
/// Uses standard Ethereum primitives, chain spec, storage, and engine types.
/// Customizes only the executor (stateful precompiles) and consensus
/// (increased extra_data size for participation bitmap).
#[derive(Clone)]
pub struct OutbeNode {
    /// Optional bridge to pass consensus data to the block executor.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Optional validator EVM signer for proposer-side system tx artifacts.
    pub evm_signer: Option<SharedOutbeEvmSigner>,
    /// Mandatory typed read-only body capabilities for execution.
    pub runtime_body_readers: RuntimeBodyReaders,
    /// Explicit CE tree owner shared with execution and finalized persistence.
    pub compressed_tree_service: std::sync::Arc<CompressedTreeService>,
}

impl std::fmt::Debug for OutbeNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeNode")
            .field("bridge", &self.bridge)
            .field(
                "evm_signer",
                &self.evm_signer.as_ref().map(|signer| signer.address()),
            )
            .field("runtime_body_readers", &"configured")
            .field("compressed_tree_service", &"configured")
            .finish()
    }
}

impl OutbeNode {
    /// Creates a new node with a consensus bridge.
    pub fn with_bridge(
        bridge: ConsensusExecutionBridge,
        runtime_body_readers: RuntimeBodyReaders,
        compressed_tree_service: std::sync::Arc<CompressedTreeService>,
    ) -> Self {
        Self {
            bridge: Some(bridge),
            evm_signer: None,
            runtime_body_readers,
            compressed_tree_service,
        }
    }

    pub fn with_bridge_and_evm_signer(
        bridge: ConsensusExecutionBridge,
        evm_signer: SharedOutbeEvmSigner,
        runtime_body_readers: RuntimeBodyReaders,
        compressed_tree_service: std::sync::Arc<CompressedTreeService>,
    ) -> Self {
        Self {
            bridge: Some(bridge),
            evm_signer: Some(evm_signer),
            runtime_body_readers,
            compressed_tree_service,
        }
    }
}

impl NodeTypes for OutbeNode {
    type Primitives = OutbePrimitives;
    type ChainSpec = ChainSpec<OutbeHeader>;
    type Storage = EthStorage<OutbeTxEnvelope, OutbeHeader>;
    type Payload = OutbePayloadTypes;
}

impl<N> Node<N> for OutbeNode
where
    N: FullNodeTypes<Types = Self>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        OutbePoolBuilder,
        BasicPayloadServiceBuilder<OutbePayloadBuilderBuilder>,
        EthereumNetworkBuilder,
        OutbeExecutorBuilder,
        OutbeConsensusBuilder,
    >;

    type AddOns = RpcAddOns<
        NodeAdapter<N>,
        EthereumEthApiBuilder,
        OutbeEngineValidatorBuilder,
        NoopEngineApiBuilder,
        BasicEngineValidatorBuilder<OutbeEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let executor = match &self.bridge {
            Some(bridge) => OutbeExecutorBuilder::with_bridge(bridge.clone()),
            None => OutbeExecutorBuilder::default(),
        };
        let executor = match &self.evm_signer {
            Some(signer) => executor.with_evm_signer(signer.clone()),
            None => executor,
        };
        let executor = executor
            .with_runtime_body_readers(self.runtime_body_readers.clone())
            .with_compressed_tree_service(self.compressed_tree_service.clone());

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(OutbePoolBuilder)
            .executor(executor)
            .payload(BasicPayloadServiceBuilder::new(OutbePayloadBuilderBuilder))
            .network(EthereumNetworkBuilder::default())
            .consensus(OutbeConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        // reth 2.1 RpcAddOns::new takes 6 args: the two trailing Identity
        // slots are rpc_middleware and auth_http_middleware (latter added in
        // 2.1). Outbe uses the upstream defaults for both.
        RpcAddOns::new(
            EthereumEthApiBuilder::default(),
            OutbeEngineValidatorBuilder::default(),
            NoopEngineApiBuilder::default(),
            BasicEngineValidatorBuilder::new(OutbeEngineValidatorBuilder::default()),
            Default::default(),
            Default::default(),
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OutbePayloadBuilderBuilder;

impl<Node, Pool> PayloadBuilderBuilder<Node, Pool, outbe_evm::OutbeEvmConfig>
    for OutbePayloadBuilderBuilder
where
    Node: FullNodeTypes<Types = OutbeNode>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = OutbeTxEnvelope>>
        + Unpin
        + 'static,
{
    type PayloadBuilder = OutbePayloadBuilder<Pool, Node::Provider>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: outbe_evm::OutbeEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        let conf = ctx.payload_builder_config();
        let chain = ctx.chain_spec().chain();
        let gas_limit = conf.gas_limit_for(chain);

        Ok(OutbePayloadBuilder::new(
            ctx.provider().clone(),
            pool,
            evm_config,
            EthereumBuilderConfig::new()
                .with_gas_limit(gas_limit)
                .with_max_blobs_per_block(conf.max_blobs_per_block())
                .with_extra_data(conf.extra_data()),
        ))
    }
}
