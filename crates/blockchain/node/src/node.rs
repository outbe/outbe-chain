//! Outbe node type definition.
//!
//! Defines `OutbeNode`. It uses Outbe-specific primitives and node components
//! on top of the Reth node builder.

use crate::{
    consensus::OutbeConsensusBuilder,
    engine::OutbeEngineValidatorBuilder,
    payload_builder::OutbePayloadBuilder,
    shutdown::{NodeShutdown, OutbePayloadServiceBuilder},
};
use std::sync::{Arc, Mutex};

use outbe_evm::{
    install_evm_runtime_host, EvmRuntimeHost, OutbeExecutorBuilder, SharedOutbeEvmSigner,
};
use outbe_primitives::{
    consensus::ConsensusExecutionBridge, OutbeHeader, OutbePayloadTypes, OutbePrimitives,
    OutbeTxEnvelope,
};
use outbe_txpool::{install_txpool_zero_fee_policy, OutbePoolBuilder, TxPoolZeroFeePolicy};
use reth_chainspec::{ChainSpec, EthChainSpec};
use reth_ethereum::node::node::EthereumEthApiBuilder;
use reth_ethereum::node::EthereumNetworkBuilder;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_node_builder::{
    components::{ComponentsBuilder, PayloadBuilderBuilder},
    node::{FullNodeTypes, NodeTypes},
    rpc::{BasicEngineValidatorBuilder, NoopEngineApiBuilder, RpcAddOns},
    BuilderContext, Node, NodeAdapter, PayloadBuilderConfig,
};
use reth_provider::EthStorage;
use reth_transaction_pool::{PoolTransaction, TransactionPool};

/// Outbe node type configuration.
///
/// Uses Outbe-specific node types: `OutbePrimitives`, a chain spec over
/// `OutbeHeader`, storage over `OutbeTxEnvelope` and `OutbeHeader`, and
/// `OutbePayloadTypes`. Customizes these components:
/// - the txpool (`OutbePoolBuilder`),
/// - the payload service (`OutbePayloadServiceBuilder`),
/// - the executor (`OutbeExecutorBuilder`, stateful precompiles),
/// - consensus (`OutbeConsensusBuilder`),
/// - the engine validator (`OutbeEngineValidatorBuilder`).
///
/// The network and the `eth` RPC API use the standard Ethereum builders.
#[derive(Clone)]
pub struct OutbeNode {
    /// Process-owned shutdown observation and payload-event lifetime.
    pub shutdown: NodeShutdown,
    /// Optional bridge to pass consensus data to the block executor.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Optional validator EVM signer for proposer-side system tx artifacts.
    pub evm_signer: Option<SharedOutbeEvmSigner>,
}

impl std::fmt::Debug for OutbeNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeNode")
            .field("bridge", &self.bridge)
            .field(
                "evm_signer",
                &self.evm_signer.as_ref().map(|signer| signer.address()),
            )
            .finish()
    }
}

impl OutbeNode {
    /// Creates a new node with a consensus bridge.
    pub fn with_bridge(bridge: ConsensusExecutionBridge) -> Self {
        Self {
            shutdown: NodeShutdown::default(),
            bridge: Some(bridge),
            evm_signer: None,
        }
    }

    pub fn with_bridge_and_evm_signer(
        bridge: ConsensusExecutionBridge,
        evm_signer: SharedOutbeEvmSigner,
    ) -> Self {
        Self {
            shutdown: NodeShutdown::default(),
            bridge: Some(bridge),
            evm_signer: Some(evm_signer),
        }
    }

    #[must_use]
    pub fn with_shutdown(mut self, shutdown: NodeShutdown) -> Self {
        self.shutdown = shutdown;
        self
    }

    /// Stashes `host` until [`Node::components_builder`] installs it.
    ///
    /// `OutbeNode` has no host field. An external struct literal constructs it,
    /// so the pending value lives in a process-wide lock.
    #[must_use]
    pub fn with_runtime_host(self, host: Arc<dyn EvmRuntimeHost>) -> Self {
        *pending_host()
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(host);
        self
    }

    /// Stashes `policy` until [`Node::components_builder`] installs it.
    ///
    /// An unset policy leaves the pool on its normal-fee default.
    #[must_use]
    pub fn with_pool_policy(self, policy: Arc<dyn TxPoolZeroFeePolicy>) -> Self {
        *pending_pool()
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(policy);
        self
    }
}

fn pending_host() -> &'static Mutex<Option<Arc<dyn EvmRuntimeHost>>> {
    static PENDING: Mutex<Option<Arc<dyn EvmRuntimeHost>>> = Mutex::new(None);
    &PENDING
}

fn pending_pool() -> &'static Mutex<Option<Arc<dyn TxPoolZeroFeePolicy>>> {
    static PENDING: Mutex<Option<Arc<dyn TxPoolZeroFeePolicy>>> = Mutex::new(None);
    &PENDING
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
        OutbePayloadServiceBuilder,
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
        let executor = match pending_host()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            Some(host) => {
                let _ = install_evm_runtime_host(Arc::clone(&host));
                executor.with_runtime_host(host)
            }
            None => executor,
        };
        if let Some(policy) = pending_pool()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            let _ = install_txpool_zero_fee_policy(policy);
        }
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(OutbePoolBuilder)
            .executor(executor)
            .payload(OutbePayloadServiceBuilder::new(self.shutdown.clone()))
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
