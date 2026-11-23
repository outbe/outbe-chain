//! Outbe EVM config - configures [`OutbeEvmFactory`], [`OutbeBlockAssembler`],
//! and [`crate::executor::OutbeBlockExecutor`] for Reth v2.11 (`evm2`).

use alloy_eips::eip7840::BlobParams;
use alloy_primitives::Bytes;
use evm2::SpecId;
use reth_ethereum::{
    chainspec::{ChainSpec, EthChainSpec},
    evm::{EthEvmEnv, RethReceiptBuilder},
};
use std::sync::Arc;

use outbe_primitives::{
    consensus::ConsensusExecutionBridge, reshare_artifact::sanitize_prefinal_outbe_block_artifacts,
    OutbeHeader,
};

use crate::{
    executor::AccountedParentArtifactProvider,
    factory::{OutbeEvmFactory, OutbeEvmTypes},
    signer::SharedOutbeEvmSigner,
};

mod parent_artifacts;
use parent_artifacts::BridgeAccountedParentArtifactProvider;
pub use parent_artifacts::RethAccountedParentArtifactProvider;

mod context;
pub use context::{OutbeBlockExecutionCtx, OutbeNextBlockEnvAttributes};

mod assembler;
pub use assembler::OutbeBlockAssembler;

pub(crate) mod system_txs;
use system_txs::system_tx_expectations_for_block;

mod factory;

mod reth_config;

mod builder;
pub use builder::OutbeExecutorBuilder;

/// Outbe EVM configuration.
///
/// Configures [`OutbeEvmFactory`] and acts as the [`reth_evm::BlockExecutorFactory`]
/// so that [`crate::executor::OutbeBlockExecutor`] processes every block, including
/// reserved-address system tx verification in the normal ordered transaction loop.
#[derive(Clone)]
pub struct OutbeEvmConfig {
    pub(crate) chain_spec: Arc<ChainSpec<OutbeHeader>>,
    pub(crate) evm_factory: OutbeEvmFactory,
    pub(crate) receipt_builder: RethReceiptBuilder,
    block_assembler: OutbeBlockAssembler,
    tee_attestation_v1: crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    /// Optional bridge to the consensus layer for finalization data.
    pub bridge: Option<ConsensusExecutionBridge>,
    accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    /// Validator-mode EVM signer used to authenticate system-tx artifacts.
    evm_signer: Option<SharedOutbeEvmSigner>,
    /// Host installed for this config. Constructors require it.
    runtime_host: Arc<dyn crate::EvmRuntimeHost>,
}

impl std::fmt::Debug for OutbeEvmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeEvmConfig")
            .field("chain_spec", &self.chain_spec)
            .field("evm_factory", &self.evm_factory)
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
            .field("runtime_host", &"installed")
            .finish()
    }
}

impl OutbeEvmConfig {
    /// Install the genesis-fixed consensus chain id into the process-wide
    /// consensus-namespace source of truth BEFORE any block
    /// execution or proof verification.
    ///
    /// EVERY `OutbeEvmConfig` constructor calls this function, so the binding is
    /// live no matter which constructor the running node uses.
    fn install_consensus_chain_id(chain_spec: &Arc<ChainSpec<OutbeHeader>>) {
        if let Err(error) =
            outbe_primitives::proof::init_consensus_chain_id(chain_spec.chain().id())
        {
            panic!("failed to bind the EVM to its consensus chain id: {error}");
        }
        static LOG_ONCE: std::sync::Once = std::sync::Once::new();
        LOG_ONCE.call_once(|| {
            tracing::info!(
                target: "outbe::evm",
                chain_id = outbe_primitives::proof::consensus_chain_id(),
                "consensus chain id bound into the signing namespace"
            );
        });
    }

    fn build(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: Option<ConsensusExecutionBridge>,
        accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
        runtime_host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self::install_consensus_chain_id(&chain_spec);
        let tee_attestation_v1 =
            crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &chain_spec,
            );
        let evm_factory = OutbeEvmFactory::new(Arc::clone(&runtime_host))
            .with_genesis_hash(chain_spec.genesis_hash())
            .with_genesis_timestamp(chain_spec.genesis_timestamp())
            .with_tee_attestation_v1(tee_attestation_v1.clone());
        let block_assembler = OutbeBlockAssembler::new(Arc::clone(&chain_spec));
        Self {
            chain_spec,
            evm_factory,
            receipt_builder: RethReceiptBuilder::default(),
            block_assembler,
            tee_attestation_v1,
            bridge,
            accounted_parent_artifact_provider,
            evm_signer: None,
            runtime_host,
        }
    }

    /// Creates a new [`OutbeEvmConfig`] with the given chain spec and host.
    pub fn new(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        runtime_host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self::build(chain_spec, None, None, runtime_host)
    }

    /// Creates a new [`OutbeEvmConfig`] with a consensus bridge.
    pub fn new_with_bridge(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
        runtime_host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        let provider: Arc<dyn AccountedParentArtifactProvider> =
            Arc::new(BridgeAccountedParentArtifactProvider::new(bridge.clone()));
        Self::build(chain_spec, Some(bridge), Some(provider), runtime_host)
    }

    fn new_with_bridge_and_summary_provider(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
        accounted_parent_artifact_provider: Arc<dyn AccountedParentArtifactProvider>,
        runtime_host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self::build(
            chain_spec,
            Some(bridge),
            Some(accounted_parent_artifact_provider),
            runtime_host,
        )
    }

    /// Full-node constructor. Installs an [`AccountedParentArtifactProvider`] backed
    /// solely by a Reth [`reth_provider::HeaderProvider`] (no consensus bridge / proof cache).
    pub fn new_with_provider_only(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        accounted_parent_artifact_provider: Arc<dyn AccountedParentArtifactProvider>,
        runtime_host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self::build(
            chain_spec,
            None,
            Some(accounted_parent_artifact_provider),
            runtime_host,
        )
    }

    /// Returns the chain spec associated with this configuration.
    pub const fn chain_spec(&self) -> &Arc<ChainSpec<OutbeHeader>> {
        &self.chain_spec
    }

    /// Stores `host` on this config and on the factory that builds its EVMs.
    #[must_use]
    pub fn with_runtime_host(mut self, host: Arc<dyn crate::EvmRuntimeHost>) -> Self {
        self.runtime_host = Arc::clone(&host);
        self.evm_factory = self.evm_factory.clone().with_runtime_host(host);
        self
    }

    pub fn with_evm_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.evm_signer = Some(signer);
        self
    }

    pub fn evm_signer(&self) -> Option<&SharedOutbeEvmSigner> {
        self.evm_signer.as_ref()
    }

    pub(crate) fn evm_env_from_base_spec(
        &self,
        spec: SpecId,
        block: evm2::env::BlockEnv,
        blob_params: Option<BlobParams>,
    ) -> EthEvmEnv<OutbeEvmTypes> {
        let mut version = self.evm_factory.version(spec, self.chain_spec.chain().id());
        if let Some(blob_params) = blob_params {
            version.max_blobs_per_tx = blob_params.max_blobs_per_tx as usize;
        }
        EthEvmEnv::new_with_version(spec, block.with_ext(()), version)
    }

    fn sanitize_next_block_extra_data(extra_data: Bytes) -> outbe_primitives::error::Result<Bytes> {
        // Builder text is not a protocol artifact. OART decode failures must reach execution validation.
        if !extra_data.starts_with(b"OART") {
            return Ok(Bytes::new());
        }
        sanitize_prefinal_outbe_block_artifacts(extra_data.as_ref())
    }
}

#[cfg(test)]
mod tests;
