use alloy_evm::eth::EthBlockExecutionCtx;

use alloy_primitives::{Address, B256};

use outbe_primitives::{
    consensus_metadata::CertifiedParentAccountingMetadata, projection::ExecutionReadBudget,
    OutbeHeader,
};

use reth_ethereum::evm::primitives::NextBlockEnvAttributes;
use reth_ethereum::TransactionSigned;

use reth_primitives_traits::Recovered;
use reth_primitives_traits::SealedHeader;

use reth_rpc_eth_api::helpers::pending_block::BuildPendingEnv;

/// Execution context for an Outbe block. The inner Ethereum context keeps the
/// EVM path unchanged; the millis remainder is only used when assembling the
/// Outbe header.
#[derive(Debug, Clone)]
pub struct OutbeBlockExecutionCtx<'a> {
    pub inner: EthBlockExecutionCtx<'a>,
    pub timestamp_millis_part: u64,
    pub block_hash: Option<B256>,
    /// State root from the sealed block header on validator/import execution.
    /// `None` while a proposer is still building the block.
    pub block_state_root: Option<B256>,
    pub expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    pub expected_end_system_txs: Vec<Recovered<TransactionSigned>>,
    pub system_layout_error: Option<String>,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub proposer_evm_address: Option<Address>,
    /// Whether Outbe consensus-critical block hooks and system txs should run.
    /// Disabled only for Reth's local pending-block RPC construction, which lacks
    /// consensus-only parent certificate and proposer context.
    pub execute_outbe_block_hooks: bool,
    /// proposer-side Phase 1 (CertifiedParentAccounting) body[0] tx
    /// signed by the payload builder BEFORE `apply_pre_execution_changes`. When
    /// set, the executor reuses it byte-for-byte as the Phase 1 commit witness
    /// (so the pre-exec receipt and the body[0] tx are guaranteed identical).
    /// `None` on the validator path (body[0] arrives through
    /// `expected_begin_system_txs`) and for `block_number <= GENESIS_BOOTSTRAP_BLOCK_NUMBER`.
    pub prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    /// optional accounted-parent artifact hint supplied by the
    /// payload builder (or import driver) when the executor's
    /// [`AccountedParentArtifactProvider`] cannot see the parent header in
    /// tree state. The executor accepts the hint ONLY when the metadata's
    /// `(finalized_block_number, finalized_block_hash)` matches
    /// `(self.parent_block_number, self.parent_hash)` and the artifact bytes
    /// decode cleanly. `None` on the validator path (provider always
    /// has the sealed block) and on the proposer path when the bridge cache
    /// already holds the artifact.
    pub parent_artifact_hint: Option<crate::executor::AccountedParentArtifact>,
    /// optional one-time Phase 3b `TeeBootstrap` payload supplied by the
    /// proposer's tribute-DKG bootstrap producer once the ceremony completes and
    /// the `TeeRegistry` is still empty. `None` on every block until then and on
    /// the validator path (the body carries the bootstrap, read via
    /// `expected_begin_system_txs`). Flows into the executor and into
    /// `build_begin_system_txs` so both proposer paths inject it identically.
    pub pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Local request budget for synchronous off-chain body reads.
    pub execution_read_budget: Option<ExecutionReadBudget>,
}

/// Attributes needed to construct the next Outbe block.
#[derive(Debug, Clone)]
pub struct OutbeNextBlockEnvAttributes {
    pub inner: NextBlockEnvAttributes,
    pub timestamp_millis_part: u64,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub proposer_evm_address: Option<Address>,
    /// False only for local pending-block RPC construction.
    pub execute_outbe_block_hooks: bool,
    /// optional prebuilt Phase 1 tx supplied by the proposer payload
    /// builder. Flows into [`OutbeBlockExecutionCtx::prebuilt_phase1_tx`].
    pub prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    /// optional accounted-parent artifact hint. Flows into
    /// [`OutbeBlockExecutionCtx::parent_artifact_hint`]; see field docs there
    /// for executor-side acceptance rules.
    pub parent_artifact_hint: Option<crate::executor::AccountedParentArtifact>,
    /// optional one-time Phase 3b `TeeBootstrap` payload from the proposer's
    /// tribute-DKG bootstrap producer. Flows into
    /// [`OutbeBlockExecutionCtx::pending_tee_bootstrap`].
    pub pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Local request budget for synchronous off-chain body reads.
    pub execution_read_budget: Option<ExecutionReadBudget>,
}

impl BuildPendingEnv<OutbeHeader> for OutbeNextBlockEnvAttributes {
    fn build_pending_env(
        parent: &SealedHeader<OutbeHeader>,
        block_overrides: Option<&alloy_rpc_types_eth::BlockOverrides>,
    ) -> Self {
        let mut inner = NextBlockEnvAttributes::build_pending_env(parent, block_overrides);
        inner.suggested_fee_recipient = outbe_primitives::addresses::REWARDS_ADDRESS;
        Self {
            inner,
            timestamp_millis_part: parent.timestamp_millis_part(),
            parent_consensus_metadata: None,
            proposer_evm_address: None,
            execute_outbe_block_hooks: false,
            prebuilt_phase1_tx: None,
            parent_artifact_hint: None,
            pending_tee_bootstrap: None,
            execution_read_budget: None,
        }
    }
}
