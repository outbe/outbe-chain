//! Phase 1 (`CertifiedParentAccounting`) verification, witness resolution, and settlement.

use alloy_consensus::Transaction as _;
use alloy_evm::{
    block::{BlockExecutionError, StateDB},
    RecoveredTx,
};
use alloy_primitives::{Address, Bytes, B256};
use outbe_primitives::committee_snapshot::committee_snapshot_key;
use outbe_primitives::proof::verify_v2_proof;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result as OutbeResult,
    storage::StorageHandle,
    system_tx::{build_unsigned_system_tx, validate_phase1_witness_against},
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::Recovered;

use crate::{
    errors::{ExecutorStorageError, ExecutorValidationError},
    executor::{context, current_preloaded_system_tx_context},
    SharedOutbeEvmSigner,
};

/// Structural sanity checks for finalized-parent consensus metadata.
///
/// `metadata.ordered_committee` is the canonical historical committee for the
/// finalized-parent certificate. The consensus/application layer already
/// verified it. This validation enforces post-exec invariants that do not require
/// the live active set:
///
/// - signer bitmap length matches committee length
/// - committee has no duplicate addresses
/// - every committee member is a registered validator (not necessarily a
///   current consensus participant - historical EXITING/UNBONDING is fine)
/// - every `missed_proposer` is a member of `metadata.ordered_committee`
/// - bitmap entries are 0 or 1 only
pub fn validate_finalized_metadata(
    host: &dyn crate::EvmRuntimeHost,
    storage: StorageHandle,
    metadata: &CertifiedParentAccountingMetadata,
) -> OutbeResult<()> {
    host.validate_finalized_metadata(storage, metadata)
}

pub use outbe_primitives::system_tx_context::AccountedParentArtifact;

/// Exact-hash-first lookup of an accounted-parent's [`ExecutionSummaryArtifact`].
pub trait AccountedParentArtifactProvider: Send + Sync {
    fn execution_summary_by_hash(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<AccountedParentArtifact>, reth_evm::execute::ProviderError>;
}

/// `CertifiedParentAccounting` system tx body: apply the immediate parent's
/// finalization facts, participation, fee settlement, and deterministic slashing.
pub fn run_finalization_and_slashing(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> OutbeResult<()> {
    let execution = current_preloaded_system_tx_context().unwrap_or_else(|| {
        outbe_primitives::system_tx_context::SystemTxExecutionContext::for_end_block(
            ctx.block.proposer,
            ctx.block.block_gas_limit,
            ctx.block.user_gas_used,
        )
    });
    host.run_finalization_and_slashing(ctx, metadata, &execution)
}

/// Input parameters for resolving the accounted-parent execution summary.
pub(crate) struct ParentArtifactLookup<'a> {
    pub provider: Option<&'a dyn AccountedParentArtifactProvider>,
    pub hint: Option<AccountedParentArtifact>,
    pub block_number: u64,
    pub parent_hash: B256,
    pub metadata: &'a CertifiedParentAccountingMetadata,
}

/// Resolves the accounted-parent artifact for the given Phase 1 metadata.
pub(crate) fn resolve_accounted_parent_artifact(
    lookup: ParentArtifactLookup<'_>,
) -> Result<AccountedParentArtifact, BlockExecutionError> {
    let ParentArtifactLookup {
        provider,
        hint,
        block_number,
        parent_hash,
        metadata,
    } = lookup;

    if let Some(provider) = provider {
        match provider.execution_summary_by_hash(
            metadata.finalized_block_number,
            metadata.finalized_block_hash,
        ) {
            Ok(Some(resolved)) => return Ok(resolved),
            Ok(None) | Err(reth_evm::execute::ProviderError::HeaderNotFound(_)) => {}
            Err(error) => {
                return Err(BlockExecutionError::other(
                    ExecutorStorageError::read_accounted_parent_artifact(error),
                ));
            }
        }
    }

    if let Some(hint) = hint {
        let parent_block_number = block_number.saturating_sub(1);
        if metadata.finalized_block_hash == parent_hash
            && metadata.finalized_block_number == parent_block_number
        {
            return Ok(hint);
        }
        return Err(ExecutorValidationError::ParentArtifactHintMismatch {
            metadata_number: metadata.finalized_block_number,
            metadata_hash: metadata.finalized_block_hash,
            parent_number: parent_block_number,
            parent_hash,
        }
        .into());
    }

    Err(BlockExecutionError::other(
        ExecutorStorageError::missing_accounted_parent_execution_summary(
            metadata.finalized_block_number,
            metadata.finalized_block_hash,
        ),
    ))
}

/// Input parameters for pre-execution Phase 1 certificate verification.
pub(crate) struct Phase1VerifyInput<'a> {
    pub block_context: BlockContext,
    pub parent_hash: B256,
    pub prevrandao: Option<B256>,
    pub metadata: &'a CertifiedParentAccountingMetadata,
}

/// Verifies the Phase 1 (`CertifiedParentAccounting`) certificate against the
/// historical committee snapshot and block `prevrandao`, returning the verified
/// canonical VRF proof hash.
pub(crate) fn verify_phase1_certificate<DB>(
    host: &dyn crate::EvmRuntimeHost,
    db: &mut DB,
    input: Phase1VerifyInput<'_>,
) -> Result<B256, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let Phase1VerifyInput {
        block_context,
        parent_hash,
        prevrandao,
        metadata,
    } = input;
    let block_number = block_context.block_number;
    let snapshot_key =
        committee_snapshot_key(metadata.finalized_epoch, metadata.committee_set_hash);
    let snapshot =
        context::read_committee_snapshot(host, db, block_context, snapshot_key, |error| {
            ExecutorValidationError::phase1_read_committee_snapshot(
                metadata.finalized_epoch,
                snapshot_key,
                error,
            )
        })?;
    let snapshot = snapshot.ok_or(ExecutorValidationError::Phase1MissingCommitteeSnapshot {
        epoch: metadata.finalized_epoch,
        snapshot_key,
    })?;

    let verified = verify_v2_proof(metadata, &snapshot, metadata.proof.as_ref(), parent_hash)
        .map_err(|error| {
            BlockExecutionError::from(ExecutorValidationError::phase1_verify_proof_rejected(
                block_number,
                error,
            ))
        })?;

    if prevrandao != Some(verified.prev_randao) {
        return Err(ExecutorValidationError::PrevrandaoMismatch {
            expected: verified.prev_randao,
            actual: prevrandao,
        }
        .into());
    }

    Ok(verified.vrf_proof_hash)
}

/// Input parameters for resolving and validating the Phase 1 signed witness transaction.
pub(crate) struct Phase1WitnessLookup<'a> {
    pub prebuilt_phase1_tx: Option<&'a Recovered<TransactionSigned>>,
    pub expected_begin_system_txs: &'a [Recovered<TransactionSigned>],
    pub evm_signer: Option<&'a SharedOutbeEvmSigner>,
    pub calldata: &'a Bytes,
    pub proposer: Address,
    pub chain_id: u64,
    pub block_number: u64,
}

/// Resolves and validates the signed Phase 1 witness transaction (`prebuilt`,
/// `body[0]`, or locally signed), returning `(tx_hash, signed_gas_limit)`.
pub(crate) fn resolve_phase1_witness(
    lookup: Phase1WitnessLookup<'_>,
) -> Result<Option<(B256, u64)>, BlockExecutionError> {
    let Phase1WitnessLookup {
        prebuilt_phase1_tx,
        expected_begin_system_txs,
        evm_signer,
        calldata,
        proposer,
        chain_id,
        block_number,
    } = lookup;

    let validate_witness = |tx: &TransactionSigned, source: &'static str| {
        validate_phase1_witness_against(tx, calldata.as_ref(), proposer, chain_id, block_number)
            .map_err(|error| {
                BlockExecutionError::from(ExecutorValidationError::phase1_commit_invalid_witness(
                    source, error,
                ))
            })
    };
    let witness = if let Some(prebuilt) = prebuilt_phase1_tx {
        let tx_hash = validate_witness(prebuilt.tx(), "prebuilt")?;
        (tx_hash, prebuilt.tx().gas_limit())
    } else if let Some(expected) = expected_begin_system_txs.first() {
        let tx_hash = validate_witness(expected.tx(), "body[0]")?;
        (tx_hash, expected.tx().gas_limit())
    } else if let Some(signer) = evm_signer {
        let unsigned = build_unsigned_system_tx(0, block_number, chain_id, calldata.clone())
            .map_err(ExecutorValidationError::phase1_commit_build_unsigned_witness)?;
        let signed = signer
            .sign_unsigned(unsigned)
            .map_err(ExecutorValidationError::phase1_commit_sign_witness)?;
        let signed_gas_limit = signed.gas_limit();
        let tx_hash = validate_witness(&signed, "signed")?;
        (tx_hash, signed_gas_limit)
    } else {
        return Ok(None);
    };
    Ok(Some(witness))
}
