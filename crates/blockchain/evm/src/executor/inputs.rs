//! Per-block construction data, separated from mutable execution state.
use super::*;

pub(crate) struct BlockExecutorInputs {
    pub(crate) identity: BlockExecutionIdentity,
    pub(crate) system_plan: BlockSystemPlan,
    pub(crate) parent_accounting: ParentAccountingInputs,
    pub(crate) dependencies: BlockExecutionDependencies,
    pub(crate) runtime: BlockExecutionRuntime,
}

pub(crate) struct BlockExecutionIdentity {
    pub(crate) block_extra_data: Bytes,
    pub(crate) validate_execution_summary: bool,
    pub(crate) block_hash: Option<B256>,
    pub(crate) block_state_root: Option<B256>,
    pub(crate) parent_hash: B256,
}

pub(crate) struct BlockSystemPlan {
    pub(crate) expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    pub(crate) expected_end_system_txs: Vec<Recovered<TransactionSigned>>,
    pub(crate) system_layout_error: Option<String>,
    pub(crate) proposer_evm_address: Option<Address>,
    pub(crate) execute_outbe_block_hooks: bool,
    pub(crate) prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    pub(crate) pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    pub(crate) ocomp_lifecycle_active: bool,
}

pub(crate) struct ParentAccountingInputs {
    pub(crate) accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    pub(crate) parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub(crate) parent_artifact_hint: Option<AccountedParentArtifact>,
}

pub(crate) struct BlockExecutionDependencies {
    pub(crate) bridge: Option<ConsensusExecutionBridge>,
    pub(crate) evm_signer: Option<SharedOutbeEvmSigner>,
}

/// Execution-local capabilities shared with this block's EVM.
pub(crate) struct BlockExecutionRuntime {
    pub(crate) compressed_entities_scope: Arc<ExecutionScope>,
    pub(crate) compressed_tree_service:
        Option<Arc<outbe_compressed_entities::CompressedTreeService>>,
    pub(crate) runtime_body_readers: Option<RuntimeBodyReaders>,
    pub(crate) execution_read_budget: Option<outbe_primitives::projection::ExecutionReadBudget>,
}
