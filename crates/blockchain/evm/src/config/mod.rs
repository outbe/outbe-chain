//! Outbe EVM config - wraps `EthEvmConfig<ChainSpec, OutbeEvmFactory>` and
//! replaces the executor factory so that every block goes through
//! [`crate::executor::OutbeBlockExecutor`] instead of [`EthBlockExecutor`] directly.

use alloy_primitives::{Bytes, B256};
use reth_ethereum::chainspec::ChainSpec;
use reth_ethereum::chainspec::EthChainSpec;
use reth_ethereum::evm::EthEvmConfig;

use std::sync::Arc;

use outbe_compressed_entities::{CompressedTreeService, ExecutionScope, ACTIVE_COMMITMENT_SCHEME};
use outbe_metadosis::api::OcompFinalizedIntentAuthority;
use outbe_metadosis::config::OcompForkInstallV1;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::{
    consensus::ConsensusExecutionBridge, reshare_artifact::sanitize_prefinal_outbe_block_artifacts,
    OutbeHeader,
};

use crate::{
    executor::AccountedParentArtifactProvider, factory::OutbeEvmFactory,
    signer::SharedOutbeEvmSigner, system_tx::OcompLifecycleActivation,
};

mod parent_artifacts;
use parent_artifacts::BridgeAccountedParentArtifactProvider;
pub use parent_artifacts::RethAccountedParentArtifactProvider;

mod finality;
use finality::ProviderAnchoredOcompFinalityAuthority;

mod context;
pub use context::{OutbeBlockExecutionCtx, OutbeNextBlockEnvAttributes};

mod assembler;
pub use assembler::OutbeBlockAssembler;

mod system_txs;
use system_txs::system_tx_expectations_for_block;

mod factory;

mod reth_config;

mod builder;
pub use builder::OutbeExecutorBuilder;

/// Outbe EVM configuration.
///
/// Wraps [`EthEvmConfig`] parametrised with [`OutbeEvmFactory`] and overrides
/// the block executor factory so that every block is processed by
/// [`crate::executor::OutbeBlockExecutor`], including reserved-address system tx verification in
/// the normal ordered transaction loop.
#[derive(Clone)]
pub struct OutbeEvmConfig {
    pub(crate) inner: EthEvmConfig<ChainSpec<OutbeHeader>, OutbeEvmFactory>,
    block_assembler: OutbeBlockAssembler,
    tee_attestation_v1: crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    /// Optional bridge to the consensus layer for finalization data.
    pub bridge: Option<ConsensusExecutionBridge>,
    accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    /// Validator-mode EVM signer used to authenticate system-tx artifacts.
    evm_signer: Option<SharedOutbeEvmSigner>,
    runtime_body_readers: Option<RuntimeBodyReaders>,
    compressed_tree_service: Option<Arc<CompressedTreeService>>,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
    ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
}

impl std::fmt::Debug for OutbeEvmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeEvmConfig")
            .field("inner", &self.inner)
            .field("block_assembler", &self.block_assembler)
            .field("tee_attestation_v1", &self.tee_attestation_v1)
            .field("bridge", &self.bridge)
            .field(
                "accounted_parent_artifact_provider",
                &self.accounted_parent_artifact_provider.is_some(),
            )
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
            .finish()
    }
}

impl OutbeEvmConfig {
    /// Install the genesis-fixed consensus chain id into the process-wide
    /// consensus-namespace source of truth BEFORE any block
    /// execution or proof verification.
    ///
    /// Called from EVERY `OutbeEvmConfig` constructor so the binding is live no
    /// matter which one the running node uses: `new_with_bridge` for the offline
    /// reth subcommands, and `new_with_bridge_and_summary_provider` /
    /// `new_with_provider_only` via [`OutbeExecutorBuilder::build_evm`] for the
    /// live validator and full node. Previously only `::new` installed it, but
    /// production never builds via `::new`, so `consensus_chain_id()` stayed at
    /// its default `0` and the signing namespace collapsed to `b"outbe" || 0` on
    /// every chain - silently disabling the cross-chain-replay binding.
    ///
    /// Reinstalling the same genesis-fixed id is idempotent. A conflicting
    /// construction is a process configuration error and fails immediately
    /// instead of silently retaining the first namespace.
    fn install_consensus_chain_id(chain_spec: &Arc<ChainSpec<OutbeHeader>>) {
        if let Err(error) = outbe_consensus::proof::init_consensus_chain_id(chain_spec.chain().id())
        {
            panic!("failed to bind the EVM to its consensus chain id: {error}");
        }
        // Surface the actually-bound id exactly once so operators can confirm the
        // namespace is chain-separated (a `0` here would mean it is degenerate).
        static LOG_ONCE: std::sync::Once = std::sync::Once::new();
        LOG_ONCE.call_once(|| {
            tracing::info!(
                target: "outbe::evm",
                chain_id = outbe_consensus::proof::consensus_chain_id(),
                "consensus chain id bound into the signing namespace"
            );
        });
    }

    /// Creates a new [`OutbeEvmConfig`] with the given chain spec.
    pub fn new(chain_spec: Arc<ChainSpec<OutbeHeader>>) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::new()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: None,
            evm_signer: None,
            runtime_body_readers: None,
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// Creates a config whose EVM factory installs typed, read-only Tribute and
    /// Nod body readers into the precompile dispatch seam.
    pub fn new_with_runtime_body_readers(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        runtime_body_readers: RuntimeBodyReaders,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::with_runtime_body_readers(runtime_body_readers.clone())
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: None,
            evm_signer: None,
            runtime_body_readers: Some(runtime_body_readers),
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// Creates a new [`OutbeEvmConfig`] with a consensus bridge.
    pub fn new_with_bridge(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let summary_cache = bridge.clone();
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::new()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: Some(bridge),
            accounted_parent_artifact_provider: Some(Arc::new(
                BridgeAccountedParentArtifactProvider::new(summary_cache),
            )),
            evm_signer: None,
            runtime_body_readers: None,
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// Creates a config with both the consensus bootstrap bridge and the
    /// mandatory runtime body readers used by receipt-visible system phases.
    pub fn new_with_bridge_and_runtime_body_readers(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
        runtime_body_readers: RuntimeBodyReaders,
    ) -> Self {
        let summary_cache = bridge.clone();
        Self::new_with_bridge_and_summary_provider(
            chain_spec,
            bridge,
            Arc::new(BridgeAccountedParentArtifactProvider::new(summary_cache)),
            runtime_body_readers,
        )
    }

    fn new_with_bridge_and_summary_provider(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
        accounted_parent_artifact_provider: Arc<dyn AccountedParentArtifactProvider>,
        runtime_body_readers: RuntimeBodyReaders,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::with_runtime_body_readers(runtime_body_readers.clone())
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: Some(bridge),
            accounted_parent_artifact_provider: Some(accounted_parent_artifact_provider),
            evm_signer: None,
            runtime_body_readers: Some(runtime_body_readers),
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// full-node constructor. Installs an
    /// [`AccountedParentArtifactProvider`] backed solely by a Reth
    /// [`reth_provider::HeaderProvider`] (no consensus bridge / proof cache). Used by
    /// `OutbeExecutorBuilder` when the node runs without a consensus bridge -
    /// e.g., a full node syncing the chain. Without this path the executor's
    /// Phase 1 lookup would fail with "missing provider" on every block, and
    /// full nodes would be unable to re-execute the chain.
    pub fn new_with_provider_only(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        accounted_parent_artifact_provider: Arc<dyn AccountedParentArtifactProvider>,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::new()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: Some(accounted_parent_artifact_provider),
            evm_signer: None,
            runtime_body_readers: None,
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// Creates the provider-backed full-node configuration with the typed
    /// runtime body readers required by production precompile execution.
    ///
    /// This is the no-bridge counterpart of the live node configuration: the
    /// exact accounted-parent artifact is resolved from the header provider,
    /// while Tribute and Nod bodies remain available through their read-only
    /// runtime capabilities.
    pub fn new_with_provider_and_runtime_body_readers(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        accounted_parent_artifact_provider: Arc<dyn AccountedParentArtifactProvider>,
        runtime_body_readers: RuntimeBodyReaders,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        Self {
            inner: EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                OutbeEvmFactory::with_runtime_body_readers(runtime_body_readers.clone())
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: Some(accounted_parent_artifact_provider),
            evm_signer: None,
            runtime_body_readers: Some(runtime_body_readers),
            compressed_tree_service: None,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            ocomp_fork_install: None,
        }
    }

    /// Returns the typed runtime body readers installed for block execution.
    #[must_use]
    pub const fn runtime_body_readers(&self) -> Option<&RuntimeBodyReaders> {
        self.runtime_body_readers.as_ref()
    }

    pub fn with_evm_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.evm_signer = Some(signer);
        self
    }

    /// Installs structural OCOMP lifecycle activation. The default remains
    /// disabled until OCM-26 supplies the canonical fresh-devnet schedule.
    pub fn with_ocomp_lifecycle_activation(mut self, activation: OcompLifecycleActivation) -> Self {
        self.inner
            .executor_factory
            .evm_factory()
            .install_ocomp_lifecycle_activation(activation);
        self.ocomp_lifecycle_activation = activation;
        self
    }

    /// Installs the immutable chain-manifest authority into every EVM created
    /// by this configuration.
    pub fn with_ocomp_fork_install(mut self, install: Arc<OcompForkInstallV1>) -> Self {
        self.inner
            .executor_factory
            .evm_factory()
            .install_ocomp_fork_install(install.clone());
        self.ocomp_fork_install = Some(install);
        self
    }

    #[must_use]
    pub const fn ocomp_lifecycle_activation(&self) -> OcompLifecycleActivation {
        self.ocomp_lifecycle_activation
    }

    #[must_use]
    pub const fn ocomp_lifecycle_active_at(&self, block_number: u64) -> bool {
        self.ocomp_lifecycle_activation.is_active_at(block_number)
    }

    /// Installs the explicitly owned CE tree service used by every block scope
    /// and by candidate publication. ADR-008's unsharded stage is not activated
    /// before ADR-009/010 benchmarking, so work accounting stays in the named
    /// prebenchmark mode below rather than inventing network limits here.
    pub fn with_compressed_tree_service(mut self, service: Arc<CompressedTreeService>) -> Self {
        self.inner
            .executor_factory
            .evm_factory()
            .install_compressed_tree_service(service.clone());
        self.compressed_tree_service = Some(service);
        self
    }

    fn with_ocomp_finality_authority(
        self,
        authority: Arc<dyn OcompFinalizedIntentAuthority>,
    ) -> Self {
        self.inner
            .executor_factory
            .evm_factory()
            .install_ocomp_finality_authority(authority);
        self
    }

    /// Returns the CE tree service shared by execution and payload cleanup.
    #[must_use]
    pub fn compressed_tree_service(&self) -> Option<Arc<CompressedTreeService>> {
        self.compressed_tree_service.clone()
    }

    fn configure_compressed_entities_scope(
        &self,
        scope: &Arc<ExecutionScope>,
        block_number: u64,
        parent_hash: B256,
    ) -> Result<(), String> {
        if let Some(service) = &self.compressed_tree_service {
            if block_number > 0 {
                scope
                    .configure_parent_tree_factory(
                        service.clone(),
                        ACTIVE_COMMITMENT_SCHEME,
                        block_number - 1,
                        parent_hash,
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub fn evm_signer(&self) -> Option<&SharedOutbeEvmSigner> {
        self.evm_signer.as_ref()
    }

    fn sanitize_next_block_extra_data(extra_data: Bytes) -> Bytes {
        sanitize_prefinal_outbe_block_artifacts(extra_data.as_ref()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
