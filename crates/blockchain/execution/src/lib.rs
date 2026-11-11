//! Outbe custom reth node.
//!
//! Defines `OutbeNode` (node type configuration) and `OutbeFullNode` (launched node type alias).
//! Re-exports components needed to build the Outbe node binary.

pub mod consensus;
pub mod engine;
pub mod errors;
pub mod node;
pub mod payload_builder;
pub mod shutdown;
pub mod state_persistence;
pub mod state_proof;
pub mod tee_call_context;
pub mod tee_canary;

pub mod tee_remote_session;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

pub use consensus::{OutbeBeaconConsensus, OutbeConsensusBuilder};
pub use engine::OutbeEngineValidatorBuilder;
pub use node::OutbeNode;
pub use outbe_evm::{OutbeEvmFactory, OutbeExecutorBuilder};
pub use outbe_txpool::OutbePoolBuilder;
pub use payload_builder::OutbePayloadBuilder;

use reth_db::DatabaseEnv;
use reth_ethereum::node::node::EthereumEthApiBuilder;
use reth_node_builder::{
    rpc::{BasicEngineValidatorBuilder, NoopEngineApiBuilder, RpcAddOns},
    FullNode, NodeAdapter, RethFullAdapter,
};

/// Adapter type for the launched Outbe node.
type OutbeNodeAdapter = NodeAdapter<RethFullAdapter<DatabaseEnv, OutbeNode>>;

/// Type alias for a launched Outbe node.
pub type OutbeFullNode = FullNode<
    OutbeNodeAdapter,
    RpcAddOns<
        OutbeNodeAdapter,
        EthereumEthApiBuilder,
        OutbeEngineValidatorBuilder,
        NoopEngineApiBuilder,
        BasicEngineValidatorBuilder<OutbeEngineValidatorBuilder>,
    >,
>;
