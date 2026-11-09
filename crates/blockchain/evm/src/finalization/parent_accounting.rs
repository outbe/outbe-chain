//! Phase 1 (`CertifiedParentAccounting`) verification, witness resolution, and settlement.

use std::collections::BTreeSet;

use alloy_consensus::Transaction as _;
use alloy_evm::{
    block::{BlockExecutionError, StateDB},
    RecoveredTx,
};
use alloy_primitives::{Address, Bytes, B256};
use outbe_consensus::proof::verify_v2_proof;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result as OutbeResult,
    storage::StorageHandle,
    system_tx::{build_unsigned_system_tx, validate_phase1_witness_against},
};
use outbe_validatorset::state::committee_snapshot_key;
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::Recovered;

use crate::{
    errors::{
        BeginBlockPrecompileError, ExecutorHookError, ExecutorStorageError, ExecutorValidationError,
    },
    executor::context,
    sys_tx_precompile::{current_preloaded_system_tx_context, read_preloaded_finalized_summary},
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
    storage: StorageHandle,
    metadata: &CertifiedParentAccountingMetadata,
) -> OutbeResult<()> {
    if metadata.signer_bitmap.len() != metadata.ordered_committee.len() {
        return Err(ExecutorHookError::SignerBitmapLengthMismatch.into());
    }

    let committee_set: BTreeSet<Address> = metadata.ordered_committee.iter().copied().collect();
    if committee_set.len() != metadata.ordered_committee.len() {
        return Err(ExecutorHookError::DuplicateCommitteeAddresses.into());
    }

    validate_committee_and_missed_proposers(storage, metadata, &committee_set)?;
    validate_binary_signer_bitmap(&metadata.signer_bitmap)
}

fn validate_committee_and_missed_proposers(
    storage: StorageHandle,
    metadata: &CertifiedParentAccountingMetadata,
    committee_set: &BTreeSet<Address>,
) -> OutbeResult<()> {
    let vs_check = outbe_validatorset::contract::ValidatorSet::new(storage);
    for addr in &metadata.ordered_committee {
        if !vs_check.is_validator(*addr)? {
            return Err(ExecutorHookError::UnregisteredCommitteeMember { address: *addr }.into());
        }
    }

    for missed in &metadata.missed_proposers {
        if !committee_set.contains(&missed.validator) {
            return Err(ExecutorHookError::MissedProposerNotInCommittee {
                validator: missed.validator,
                view: missed.view,
            }
            .into());
        }
    }

    Ok(())
}

fn validate_binary_signer_bitmap(signer_bitmap: &[u8]) -> OutbeResult<()> {
    for entry in signer_bitmap {
        if *entry > 1 {
            return Err(ExecutorHookError::NonBinarySignerBitmapEntry.into());
        }
    }
    Ok(())
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
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> OutbeResult<()> {
    let expected_parent_number = validate_accounting_parent(ctx, metadata)?;
    validate_finalized_metadata(ctx.storage.clone(), metadata)?;

    let finalized = read_preloaded_finalized_summary(&ctx.storage)?.ok_or(
        BeginBlockPrecompileError::MissingPreloadedExecutionSummary {
            block_number: metadata.finalized_block_number,
            block_hash: metadata.finalized_block_hash,
        },
    )?;

    let is_fresh = verify_metadata_progress_and_fingerprint(
        ctx,
        metadata,
        &finalized,
        expected_parent_number,
    )?;
    if !is_fresh {
        return Ok(());
    }

    let voters = collect_signing_voters(metadata);
    outbe_rewards::finalized_metadata_hook::record_votes_and_fees(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        finalized.timestamp,
        &voters,
    )?;

    let missed_validators: Vec<Address> = metadata
        .missed_proposers
        .iter()
        .map(|m| m.validator)
        .collect();
    outbe_slashindicator::hooks::slash_window_proposers(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
        &missed_validators,
    )?;
    outbe_slashindicator::hooks::prune_slash_guards(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
    )?;
    outbe_cycle::record_phase1_progress(ctx, expected_parent_number)?;

    Ok(())
}

fn verify_metadata_progress_and_fingerprint(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
    finalized: &AccountedParentArtifact,
    expected_parent_number: u64,
) -> OutbeResult<bool> {
    let canonical_vrf_proof_hash = current_preloaded_system_tx_context()
        .map(|context| context.canonical_vrf_proof_hash)
        .unwrap_or(B256::ZERO);

    let last_accounted = outbe_cycle::read_last_accounted_block_number(ctx)?;
    match outbe_rewards::runtime::check_and_record_metadata_fingerprint(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        canonical_vrf_proof_hash,
    )? {
        outbe_rewards::runtime::MetadataFingerprintOutcome::IdenticalReplay => {
            if last_accounted != expected_parent_number {
                return Err(
                    BeginBlockPrecompileError::IdenticalReplayUnexpectedProgress {
                        last_accounted,
                        expected: expected_parent_number,
                    }
                    .into(),
                );
            }
            tracing::warn!(
                target: "outbe::system_tx",
                block_number = ctx.block.block_number,
                parent_block_number = expected_parent_number,
                "CertifiedParentAccounting identical replay accepted",
            );
            Ok(false)
        }
        outbe_rewards::runtime::MetadataFingerprintOutcome::Fresh => {
            let required_previous = expected_parent_number.saturating_sub(1);
            if last_accounted != required_previous {
                return Err(BeginBlockPrecompileError::ProgressGap {
                    last_accounted,
                    expected_previous: required_previous,
                    accounting_parent: expected_parent_number,
                }
                .into());
            }
            Ok(true)
        }
    }
}

fn collect_signing_voters(metadata: &CertifiedParentAccountingMetadata) -> Vec<Address> {
    let mut voters = Vec::new();
    for (addr, did_sign) in metadata
        .ordered_committee
        .iter()
        .copied()
        .zip(metadata.signer_bitmap.iter().copied())
    {
        if did_sign == 1 {
            voters.push(addr);
        }
    }
    voters
}

fn validate_accounting_parent(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> OutbeResult<u64> {
    if !ctx.block.regime().has_parent_finalization_accounting() {
        return Err(BeginBlockPrecompileError::RequiresBlockNumberAtLeastTwo.into());
    }
    let expected_parent_number = ctx
        .block
        .block_number
        .checked_sub(1)
        .ok_or(BeginBlockPrecompileError::BlockNumberUnderflow)?;
    if metadata.finalized_block_number != expected_parent_number {
        return Err(BeginBlockPrecompileError::TargetImmediateParentMismatch {
            expected: expected_parent_number,
            actual: metadata.finalized_block_number,
        }
        .into());
    }
    if metadata.finalized_block_hash.is_zero() {
        return Err(BeginBlockPrecompileError::ZeroFinalizedBlockHash.into());
    }

    Ok(expected_parent_number)
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
    let snapshot = context::read_committee_snapshot(db, block_context, snapshot_key, |error| {
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
