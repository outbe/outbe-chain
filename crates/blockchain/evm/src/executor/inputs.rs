//! Per-block construction data, separated from mutable execution state.
use std::sync::Arc;

use super::*;

pub struct BlockExecutorInputs {
    pub identity: BlockExecutionIdentity,
    pub system_plan: BlockSystemPlan,
    pub parent_accounting: ParentAccountingInputs,
    pub dependencies: BlockExecutionDependencies,
}

pub struct BlockExecutionIdentity {
    pub block_extra_data: Bytes,
    pub validate_execution_summary: bool,
    pub block_hash: Option<B256>,
    pub block_state_root: Option<B256>,
    pub parent_hash: B256,
}

pub struct BlockSystemPlan {
    pub expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    pub system_layout_error: Option<crate::errors::SystemTxExpectationError>,
    pub proposer_evm_address: Option<Address>,
    pub execute_outbe_block_hooks: bool,
    pub prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    pub pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
}

pub struct ParentAccountingInputs {
    pub accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub parent_artifact_hint: Option<AccountedParentArtifact>,
}

pub struct BlockExecutionDependencies {
    pub bridge: Option<ConsensusExecutionBridge>,
    pub evm_signer: Option<SharedOutbeEvmSigner>,
    pub runtime_host: Arc<dyn crate::EvmRuntimeHost>,
}
