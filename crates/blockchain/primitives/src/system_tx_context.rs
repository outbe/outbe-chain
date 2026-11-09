//! Neutral execution context passed into system-transaction precompiles.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use crate::reshare_artifact::ExecutionSummaryArtifact;

/// Parent-block execution artifact paired with the parent block's timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountedParentArtifact {
    pub summary: ExecutionSummaryArtifact,
    /// Parent header timestamp.
    pub timestamp: u64,
    /// State root committed by the same parent header.
    pub state_root: Option<B256>,
}

/// Execution context supplied beside system-transaction calldata.
///
/// Money fields stay out of signed calldata. The executor installs this value
/// for one system call and restores the previous value on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTxExecutionContext {
    pub proposer: Address,
    pub finalized_summary: Option<AccountedParentArtifact>,
    /// True only for a boundary block whose target set includes `proposer`.
    pub allow_boundary_proposer: bool,
    /// `keccak256` of the VRF proof in the verified parent certificate.
    ///
    /// `B256::ZERO` when that preflight did not run.
    pub canonical_vrf_proof_hash: B256,
    /// Pre-verified signer indices per late-finalize batch.
    pub verified_late_credit_signers: Option<Arc<Vec<Vec<usize>>>>,
    /// Block gas limit for user transactions.
    pub block_gas_limit: u64,
    /// Cumulative gas consumed by user transactions before end-block.
    pub user_gas_used: u64,
}

impl SystemTxExecutionContext {
    /// Context for an end-block call that carries no parent accounting artifact.
    pub const fn for_end_block(
        proposer: Address,
        block_gas_limit: u64,
        user_gas_used: u64,
    ) -> Self {
        Self {
            proposer,
            finalized_summary: None,
            allow_boundary_proposer: false,
            canonical_vrf_proof_hash: B256::ZERO,
            verified_late_credit_signers: None,
            block_gas_limit,
            user_gas_used,
        }
    }
}
