//! Neutral execution context passed into system-transaction precompiles.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256};

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

/// Pre-verified signer indices per late-finalize batch.
pub type VerifiedLateCreditSigners = Vec<Vec<usize>>;

/// Execution context supplied beside system-transaction calldata.
///
/// Money fields stay out of signed calldata. The executor installs this value
/// for one system call and restores the previous value on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTxExecutionContext {
    pub proposer: Address,
    pub finalized_summary: Option<AccountedParentArtifact>,
    /// `keccak256` of the VRF proof in the verified parent certificate.
    ///
    /// `B256::ZERO` when that preflight did not run.
    pub canonical_vrf_proof_hash: B256,
    /// Pre-verified signer indices per late-finalize batch.
    pub verified_late_credit_signers: Option<Arc<VerifiedLateCreditSigners>>,
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
            canonical_vrf_proof_hash: B256::ZERO,
            verified_late_credit_signers: None,
            block_gas_limit,
            user_gas_used,
        }
    }
}

use crate::{
    block::{BlockContext, BlockHeightRegime},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::PrecompileError,
    reshare_artifact::OutbeBlockArtifacts,
    system_tx::binding::{
        BoundaryBindingError, LateCreditsBindingError, ParentAccountingBindingError,
        PublicationBindingError,
    },
    tee_attestation_v1::{CodecError as TeeBootstrapCodecError, TeePolicyV1},
    tee_bootstrap_v2::TeeBootstrapV2,
};

/// Flat per-call context; block and gas facts remain in `BlockContext`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemTxContext {
    pub payload: Bytes,
    pub finalized_summary: Option<AccountedParentArtifact>,
    pub canonical_vrf_proof_hash: B256,
    pub verified_late_credit_signers: Option<Arc<VerifiedLateCreditSigners>>,
    pub chain_spec_tee_policy: Option<TeePolicyV1>,
}

impl SystemTxContext {
    pub fn from_preloaded(
        payload: Bytes,
        preloaded: Option<SystemTxExecutionContext>,
        chain_spec_tee_policy: Option<TeePolicyV1>,
    ) -> Self {
        let mut context = Self {
            payload,
            chain_spec_tee_policy,
            ..Self::default()
        };
        if let Some(preloaded) = preloaded {
            context.finalized_summary = preloaded.finalized_summary;
            context.canonical_vrf_proof_hash = preloaded.canonical_vrf_proof_hash;
            context.verified_late_credit_signers = preloaded.verified_late_credit_signers;
        }
        context
    }

    pub fn to_execution_context(&self, block: &BlockContext) -> SystemTxExecutionContext {
        SystemTxExecutionContext {
            proposer: block.proposer,
            finalized_summary: self.finalized_summary,
            canonical_vrf_proof_hash: self.canonical_vrf_proof_hash,
            verified_late_credit_signers: self.verified_late_credit_signers.clone(),
            block_gas_limit: block.block_gas_limit,
            user_gas_used: block.user_gas_used,
        }
    }
}

/// Inputs used to assemble system transactions.
#[derive(Debug, Clone, Copy)]
pub struct SystemTxBuildContext<'a> {
    pub regime: BlockHeightRegime,
    pub block_number: u64,
    pub parent_hash: B256,
    pub artifacts: &'a OutbeBlockArtifacts,
    pub parent_consensus_metadata: Option<&'a CertifiedParentAccountingMetadata>,
    pub pending_tee_bootstrap: Option<&'a TeeBootstrapV2>,
    pub expected_tee_policy: Option<&'a TeePolicyV1>,
}

/// Inputs passed to `ProtocolModule::validate_payload` during stateless block binding validation.
#[derive(Debug, Clone, Copy)]
pub struct SystemTxValidationContext<'a> {
    pub regime: BlockHeightRegime,
    pub block_number: u64,
    pub parent_hash: B256,
    pub artifacts: &'a OutbeBlockArtifacts,
}

#[derive(Debug, thiserror::Error)]
pub enum SystemTxPayloadBuildError {
    #[error("SteadyState block {block_number} requires parent_consensus_metadata for CertifiedParentAccounting")]
    MissingParentConsensusMetadata { block_number: u64 },
    #[error("CertifiedParentAccounting metadata hash {metadata_hash} does not match block parent_hash {parent_hash}")]
    CertifiedParentAccountingHashMismatch {
        parent_hash: B256,
        metadata_hash: B256,
    },
    #[error("GenesisBootstrap (block 1) requires ConsensusHeaderArtifact::BoundaryOutcome")]
    MissingBlock1BoundaryOutcome,
    #[error("GenesisBootstrap (block 1) requires pending TeeBootstrap payload")]
    MissingBlock1TeeBootstrapPayload,
    #[error("GenesisBootstrap (block 1) missing expected ChainSpec TEE policy")]
    MissingBlock1ExpectedTeePolicy,
    #[error("GenesisBootstrap (block 1) TeeBootstrap payload does not match ChainSpec policy")]
    Block1TeeBootstrapPolicyScheduleMismatch,
    #[error("GenesisBootstrap (block 1) TeeBootstrap payload failed preflight: {0}")]
    InvalidBlock1TeeBootstrapPayload(#[from] TeeBootstrapCodecError),
    #[error("TeeBootstrap payload is forbidden at block {block_number}")]
    TeeBootstrapForbiddenAtBlock { block_number: u64 },
    #[error("missing RadiclePublication batch for active publication step")]
    MissingPublicationBatch,
    #[error("encode system tx payload: {0}")]
    Encode(#[from] PrecompileError),
}

#[derive(Debug, thiserror::Error)]
pub enum SystemTxPayloadValidationError {
    #[error("unexpected non-empty system tx payload for {module} ({len} bytes)")]
    UnexpectedNonEmptyPayload { module: &'static str, len: usize },
    #[error(transparent)]
    ParentAccounting(#[from] ParentAccountingBindingError),
    #[error(transparent)]
    LateCredits(#[from] LateCreditsBindingError),
    #[error(transparent)]
    Boundary(#[from] BoundaryBindingError),
    #[error(transparent)]
    Publication(#[from] PublicationBindingError),
    #[error("invalid TeeBootstrap payload at block {block_number}: {source}")]
    TeeBootstrap {
        block_number: u64,
        #[source]
        source: TeeBootstrapCodecError,
    },
}
