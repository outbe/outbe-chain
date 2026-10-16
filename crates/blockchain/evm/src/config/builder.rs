use crate::signer::SharedOutbeEvmSigner;

use outbe_primitives::{consensus::ConsensusExecutionBridge, OutbeHeader, OutbePrimitives};
use reth_ethereum::{
    chainspec::ChainSpec,
    node::{
        api::{FullNodeTypes, NodeTypes},
        builder::{components::ExecutorBuilder, BuilderContext},
    },
};

use reth_provider::{BlockHashReader, BlockIdReader, HeaderProvider};
use std::sync::Arc;

use super::{OutbeEvmConfig, RethAccountedParentArtifactProvider};

// ---------------------------------------------------------------------------
// ExecutorBuilder
// ---------------------------------------------------------------------------

/// Executor builder that wires [`OutbeEvmConfig`] as the node's EVM config.
#[derive(Clone, Default)]
pub struct OutbeExecutorBuilder {
    /// Optional bridge to the consensus layer, injected by the node binary.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Optional validator EVM signer, injected by the node binary in validator mode.
    pub evm_signer: Option<SharedOutbeEvmSigner>,
}

impl std::fmt::Debug for OutbeExecutorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeExecutorBuilder")
            .field("bridge", &self.bridge)
            .field(
                "evm_signer",
                &self.evm_signer.as_ref().map(|signer| signer.address()),
            )
            .finish()
    }
}

impl OutbeExecutorBuilder {
    /// Creates a new builder with a consensus bridge.
    pub fn with_bridge(bridge: ConsensusExecutionBridge) -> Self {
        Self {
            bridge: Some(bridge),
            evm_signer: None,
        }
    }

    pub fn with_evm_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.evm_signer = Some(signer);
        self
    }
}

impl<Node> ExecutorBuilder<Node> for OutbeExecutorBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<ChainSpec = ChainSpec<OutbeHeader>, Primitives = OutbePrimitives>,
    >,
    Node::Provider: HeaderProvider<Header = OutbeHeader>
        + BlockHashReader
        + BlockIdReader
        + Clone
        + Send
        + Sync
        + 'static,
{
    type EVM = OutbeEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        // always install a provider-backed
        // `AccountedParentArtifactProvider` so the executor can resolve the
        // parent artifact in both bridge mode (cache + provider) and full-node
        // mode (provider only).
        let config = match self.bridge {
            Some(bridge) => {
                let summary_cache = bridge.clone();
                OutbeEvmConfig::new_with_bridge_and_summary_provider(
                    ctx.chain_spec(),
                    bridge,
                    Arc::new(RethAccountedParentArtifactProvider::new(
                        ctx.provider().clone(),
                        Some(summary_cache),
                    )),
                )
            }
            None => OutbeEvmConfig::new_with_provider_only(
                ctx.chain_spec(),
                Arc::new(RethAccountedParentArtifactProvider::new(
                    ctx.provider().clone(),
                    None,
                )),
            ),
        };

        Ok(match self.evm_signer {
            Some(signer) => config.with_evm_signer(signer),
            None => config,
        })
    }
}
