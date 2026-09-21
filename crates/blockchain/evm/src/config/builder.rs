use crate::{signer::SharedOutbeEvmSigner, system_tx::OcompLifecycleActivation};

use outbe_compressed_entities::CompressedTreeService;
use outbe_metadosis::api::OcompFinalizedIntentAuthority;
use outbe_metadosis::config::OcompForkInstallV1;
use outbe_offchain_data::RuntimeBodyReaders;
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

use super::{
    OutbeEvmConfig, ProviderAnchoredOcompFinalityAuthority, RethAccountedParentArtifactProvider,
};

// ---------------------------------------------------------------------------
// ExecutorBuilder
// ---------------------------------------------------------------------------

/// Executor builder that wires up [`OutbeEvmConfig`] as the node's EVM config.
#[derive(Clone, Default)]
pub struct OutbeExecutorBuilder {
    /// Optional bridge to the consensus layer, injected by the node binary.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Optional validator EVM signer, injected by the node binary in validator mode.
    pub evm_signer: Option<SharedOutbeEvmSigner>,
    /// Required read-only Tribute and Nod body authority for live execution.
    pub runtime_body_readers: Option<RuntimeBodyReaders>,
    /// Explicit CE tree owner; mandatory for live execution.
    pub compressed_tree_service: Option<Arc<CompressedTreeService>>,
    /// Inert until the canonical OCM-26 devnet schedule is supplied.
    pub ocomp_lifecycle_activation: OcompLifecycleActivation,
    /// Complete immutable authority installed at the activation height.
    pub ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
    /// Production finalized JobIntent proof authority supplied by the node.
    pub ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
}

impl std::fmt::Debug for OutbeExecutorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeExecutorBuilder")
            .field("bridge", &self.bridge)
            .field(
                "evm_signer",
                &self.evm_signer.as_ref().map(|signer| signer.address()),
            )
            .field("runtime_body_readers", &self.runtime_body_readers.is_some())
            .field(
                "compressed_tree_service",
                &self.compressed_tree_service.is_some(),
            )
            .field(
                "ocomp_lifecycle_activation",
                &self.ocomp_lifecycle_activation,
            )
            .field("ocomp_fork_install", &self.ocomp_fork_install.is_some())
            .field(
                "ocomp_finality_authority",
                &self.ocomp_finality_authority.is_some(),
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
            runtime_body_readers: None,
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
            ocomp_finality_authority: None,
        }
    }

    pub fn with_evm_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.evm_signer = Some(signer);
        self
    }

    /// Installs the mandatory read-only Tribute and Nod body bundle.
    pub fn with_runtime_body_readers(mut self, readers: RuntimeBodyReaders) -> Self {
        self.runtime_body_readers = Some(readers);
        self
    }

    pub fn with_compressed_tree_service(mut self, service: Arc<CompressedTreeService>) -> Self {
        self.compressed_tree_service = Some(service);
        self
    }

    pub fn with_ocomp_lifecycle_activation(mut self, activation: OcompLifecycleActivation) -> Self {
        self.ocomp_lifecycle_activation = activation;
        self
    }

    pub fn with_ocomp_fork_install(mut self, install: Arc<OcompForkInstallV1>) -> Self {
        self.ocomp_fork_install = Some(install);
        self
    }

    pub fn with_ocomp_finality_authority(
        mut self,
        authority: Arc<dyn OcompFinalizedIntentAuthority>,
    ) -> Self {
        self.ocomp_finality_authority = Some(authority);
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
        match (&self.ocomp_fork_install, self.ocomp_lifecycle_activation) {
            (Some(install), OcompLifecycleActivation::AtBlock(height))
                if install.activation_height == height => {}
            (None, OcompLifecycleActivation::Disabled) => {}
            (Some(_), OcompLifecycleActivation::Disabled) => {
                return Err(eyre::eyre!(
                    "OCOMP fork install requires an active lifecycle schedule"
                ));
            }
            (None, OcompLifecycleActivation::AtBlock(_)) => {
                return Err(eyre::eyre!(
                    "active OCOMP lifecycle requires a chain-manifest fork install"
                ));
            }
            (Some(install), OcompLifecycleActivation::AtBlock(height)) => {
                return Err(eyre::eyre!(
                    "OCOMP fork install height {} differs from lifecycle height {height}",
                    install.activation_height
                ));
            }
        }
        let runtime_body_readers = self.runtime_body_readers.ok_or_else(|| {
            eyre::eyre!("live Outbe EVM construction requires RuntimeBodyReaders")
        })?;
        let compressed_tree_service = self.compressed_tree_service.ok_or_else(|| {
            eyre::eyre!("live Outbe EVM construction requires CompressedTreeService")
        })?;
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
                    runtime_body_readers,
                )
            }
            None => OutbeEvmConfig::new_with_provider_and_runtime_body_readers(
                ctx.chain_spec(),
                Arc::new(RethAccountedParentArtifactProvider::new(
                    ctx.provider().clone(),
                    None,
                )),
                runtime_body_readers,
            ),
        };

        let mut config = config
            .with_compressed_tree_service(compressed_tree_service)
            .with_ocomp_lifecycle_activation(self.ocomp_lifecycle_activation);
        if let Some(install) = self.ocomp_fork_install {
            config = config.with_ocomp_fork_install(install);
        }
        if let Some(authority) = self.ocomp_finality_authority {
            config = config.with_ocomp_finality_authority(Arc::new(
                ProviderAnchoredOcompFinalityAuthority::new(ctx.provider().clone(), authority),
            ));
        }
        Ok(match self.evm_signer {
            Some(signer) => config.with_evm_signer(signer),
            None => config,
        })
    }
}
