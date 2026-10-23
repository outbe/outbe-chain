//! Outbe EVM config - wraps `EthEvmConfig<ChainSpec, OutbeEvmFactory>` and
//! replaces the executor factory so that every block goes through
//! [`crate::executor::OutbeBlockExecutor`] instead of [`EthBlockExecutor`] directly.

use alloy_primitives::Bytes;
use reth_ethereum::chainspec::ChainSpec;
use reth_ethereum::chainspec::EthChainSpec;
use reth_ethereum::evm::EthEvmConfig;

use std::sync::Arc;

use outbe_primitives::{
    consensus::ConsensusExecutionBridge, reshare_artifact::sanitize_prefinal_outbe_block_artifacts,
    OutbeHeader,
};

use crate::{
    executor::AccountedParentArtifactProvider, factory::OutbeEvmFactory,
    signer::SharedOutbeEvmSigner,
};

mod parent_artifacts;
use parent_artifacts::BridgeAccountedParentArtifactProvider;
pub use parent_artifacts::RethAccountedParentArtifactProvider;

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
/// the block executor factory. As a result, [`crate::executor::OutbeBlockExecutor`]
/// processes every block, including reserved-address system tx verification in
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
            .finish()
    }
}

impl OutbeEvmConfig {
    /// Install the genesis-fixed consensus chain id into the process-wide
    /// consensus-namespace source of truth BEFORE any block
    /// execution or proof verification.
    ///
    /// EVERY `OutbeEvmConfig` constructor calls this function, so the binding is
    /// live no matter which constructor the running node uses. The offline reth
    /// subcommands use `new_with_bridge`. The live validator and full node use
    /// `new_with_bridge_and_summary_provider` /
    /// `new_with_provider_only` via
    /// [`OutbeExecutorBuilder::build_evm`]. Previously only `::new` installed it,
    /// but production never builds via `::new`. As a result, `consensus_chain_id()`
    /// stayed at its default `0`, and the signing namespace collapsed to
    /// `b"outbe" || 0` on every chain. This silently disabled the
    /// cross-chain-replay binding.
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
                OutbeEvmFactory::default()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: None,
            evm_signer: None,
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
                OutbeEvmFactory::default()
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
        }
    }

    fn new_with_bridge_and_summary_provider(
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        bridge: ConsensusExecutionBridge,
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
                OutbeEvmFactory::default()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: Some(bridge),
            accounted_parent_artifact_provider: Some(accounted_parent_artifact_provider),
            evm_signer: None,
        }
    }

    /// Full-node constructor. Installs an
    /// [`AccountedParentArtifactProvider`] backed solely by a Reth
    /// [`reth_provider::HeaderProvider`] (no consensus bridge / proof cache).
    /// It installs no runtime body readers. `OutbeExecutorBuilder` does not use
    /// it. The no-bridge production path uses
    /// `new_with_provider_only`. Without a provider, the
    /// executor's Phase 1 lookup would fail with "missing provider" on every
    /// block.
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
                OutbeEvmFactory::default()
                    .with_genesis_hash(chain_spec.genesis_hash())
                    .with_tee_attestation_v1(tee_attestation_v1.clone()),
            ),
            block_assembler: OutbeBlockAssembler::new(chain_spec),
            tee_attestation_v1,
            bridge: None,
            accounted_parent_artifact_provider: Some(accounted_parent_artifact_provider),
            evm_signer: None,
        }
    }

    pub fn with_evm_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.evm_signer = Some(signer);
        self
    }

    pub fn evm_signer(&self) -> Option<&SharedOutbeEvmSigner> {
        self.evm_signer.as_ref()
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
