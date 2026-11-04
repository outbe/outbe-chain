//! Phase 1 (`CertifiedParentAccounting`) verification, pre-execution commit, and settlement.

use std::collections::BTreeSet;

use alloy_consensus::Transaction as _;
use alloy_evm::{
    block::{BlockExecutionError, StateDB},
    eth::EthTxResult,
    revm::context::Block as _,
    RecoveredTx,
};
use alloy_primitives::{Address, Bytes, B256};
use outbe_consensus::proof::verify_v2_proof;
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result as OutbeResult,
    reshare_artifact::{ExecutionSummaryArtifact, OutbeBlockArtifacts},
    storage::StorageHandle,
    system_tx::{
        build_unsigned_system_tx, validate_phase1_witness_against, SystemTx, SystemTxPhase,
    },
};
use outbe_validatorset::state::committee_snapshot_key;
use reth_ethereum::{
    evm::{primitives::Evm, revm::context::TxEnv},
    TransactionSigned,
};
use revm::context::result::HaltReason;

use crate::{
    begin_block_precompile::{
        current_preloaded_system_tx_context, read_preloaded_finalized_summary,
        with_preloaded_system_tx_context, PreloadedSystemTxContext,
    },
    errors::{
        BeginBlockPrecompileError, ExecutorHookError, ExecutorStorageError, ExecutorValidationError,
    },
    executor::{context, OutbeBlockExecutor, ZeroFeeCfgAccess},
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

    for entry in &metadata.signer_bitmap {
        if *entry > 1 {
            return Err(ExecutorHookError::NonBinarySignerBitmapEntry.into());
        }
    }

    Ok(())
}

/// Parent-block execution artifact (`ExecutionSummaryArtifact`
/// from `header.extra_data`) paired with the parent block's timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountedParentArtifact {
    pub summary: ExecutionSummaryArtifact,
    pub timestamp: u64,
    /// State root committed by the same exact parent header.
    pub state_root: Option<B256>,
}

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
            return Ok(());
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
        }
    }

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

    outbe_rewards::finalized_metadata_hook::on_finalized_metadata(
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

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    /// Resolves the accounted-parent artifact for the given Phase 1 metadata.
    pub(crate) fn accounted_parent_artifact_for_metadata(
        &self,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<AccountedParentArtifact, BlockExecutionError> {
        if let Some(provider) = self.accounted_parent_artifact_provider.as_ref() {
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

        if let Some(hint) = self.parent_artifact_hint.as_ref() {
            let block_number = self.inner.evm.block().number().saturating_to::<u64>();
            let parent_block_number = block_number.saturating_sub(1);
            if metadata.finalized_block_hash == self.parent_hash
                && metadata.finalized_block_number == parent_block_number
            {
                return Ok(*hint);
            }
            return Err(ExecutorValidationError::ParentArtifactHintMismatch {
                metadata_number: metadata.finalized_block_number,
                metadata_hash: metadata.finalized_block_hash,
                parent_number: parent_block_number,
                parent_hash: self.parent_hash,
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

    /// Resolves and caches the Phase-1 (`CertifiedParentAccounting`) input and
    /// parent execution summary once per block so `verify_phase1_in_preexec`,
    /// `apply_phase1_commit_in_preexec`, and `begin_block_system_tx_inputs`
    /// never duplicate the parent DB lookup.
    pub(crate) fn ensure_phase1_input(
        &mut self,
    ) -> Result<(SystemTx, AccountedParentArtifact), BlockExecutionError> {
        if let Some(cached) = self.cached_phase1_input.as_ref() {
            return Ok(cached.clone());
        }
        let (input, summary_opt) = self.certified_parent_begin_input(0)?;
        let summary = summary_opt.expect("certified_parent_begin_input always resolves summary");
        self.cached_phase1_input = Some((input.clone(), summary));
        Ok((input, summary))
    }
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    /// V2 Phase 1 preflight for `BlockHeightRegime::SteadyState` (`block_number >= 2`).
    pub(crate) fn verify_phase1_in_preexec(
        &mut self,
        block_number: u64,
        _block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_parent_finalization_accounting() {
            if regime.is_genesis_bootstrap()
                && self.inner.evm.block().prevrandao() != Some(B256::ZERO)
            {
                return Err(ExecutorValidationError::GenesisChildPrevrandaoMustBeZero.into());
            }
            return Ok(());
        }
        #[cfg(test)]
        if crate::executor::PHASE1_VERIFY_DISABLED.with(|cell| cell.get()) {
            return Ok(());
        }

        let (input, _summary) = self.ensure_phase1_input()?;
        let SystemTx::CertifiedParentAccounting { metadata } = &input else {
            return Err(
                ExecutorValidationError::Phase1VerifierExpectedCertifiedParentAccountingInput
                    .into(),
            );
        };

        let snapshot_key =
            committee_snapshot_key(metadata.finalized_epoch, metadata.committee_set_hash);
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let parent_hash = self.parent_hash;
        let cert_bytes = metadata.proof.clone();
        let metadata_for_verify = metadata.clone();

        let ctx = self.block_context(block_number, timestamp, proposer);
        let snapshot = context::read_committee_snapshot(
            self.inner.evm.db_mut(),
            ctx,
            snapshot_key,
            |error| {
                ExecutorValidationError::phase1_read_committee_snapshot(
                    metadata_for_verify.finalized_epoch,
                    snapshot_key,
                    error,
                )
            },
        )?;
        let snapshot = snapshot.ok_or(ExecutorValidationError::Phase1MissingCommitteeSnapshot {
            epoch: metadata_for_verify.finalized_epoch,
            snapshot_key,
        })?;

        let verified = verify_v2_proof(
            &metadata_for_verify,
            &snapshot,
            cert_bytes.as_ref(),
            parent_hash,
        )
        .map_err(|error| {
            BlockExecutionError::from(ExecutorValidationError::phase1_verify_proof_rejected(
                block_number,
                error,
            ))
        })?;

        let actual = self.inner.evm.block().prevrandao();
        if actual != Some(verified.prev_randao) {
            return Err(ExecutorValidationError::PrevrandaoMismatch {
                expected: verified.prev_randao,
                actual,
            }
            .into());
        }

        self.verified_phase1_vrf_proof_hash = Some(verified.vrf_proof_hash);

        Ok(())
    }

    /// Executes and commits the Phase 1 (`CertifiedParentAccounting`) system tx
    /// in pre-execution so `run_outbe_pre_execution_hooks` observes post-Phase-1 state.
    pub(crate) fn apply_phase1_commit_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_parent_finalization_accounting() {
            return Ok(());
        }
        #[cfg(test)]
        if crate::executor::PHASE1_VERIFY_DISABLED.with(|cell| cell.get()) {
            return Ok(());
        }

        let (input, finalized_summary) = self.ensure_phase1_input()?;
        if !matches!(input, SystemTx::CertifiedParentAccounting { .. }) {
            return Err(
                ExecutorValidationError::Phase1CommitUnexpectedKind { kind: input.name() }.into(),
            );
        }
        let calldata = input
            .encode()
            .map_err(ExecutorValidationError::phase1_commit_encode_input)?;

        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());

        let Some((cached_tx_hash, signed_gas_limit)) =
            self.resolve_phase1_witness(&calldata, proposer, block_number)?
        else {
            return Ok(());
        };
        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary: Some(finalized_summary),
            allow_boundary_proposer: self.boundary_allows_proposer(block_artifacts, proposer),
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
            verified_late_credit_signers: None,
        };

        let transact_outcome = with_preloaded_system_tx_context(phase_context, || {
            self.inner.evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                calldata,
            )
        });
        let result = transact_outcome.map_err(|error| {
            let reason = format!("Phase 1 commit pre-exec: transact_system_call failed: {error}");
            if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_none()
                && outbe_primitives::projection::NodeLocalExecutionAbort::find(&error).is_none()
            {
                tracing::error!(target: "outbe::executor", %reason);
            }
            BlockExecutionError::other(error)
        })?;
        if !result.result.is_success() {
            let err = ExecutorValidationError::phase1_commit_did_not_succeed(&result.result);
            tracing::error!(target: "outbe::executor", %err, "critical begin-zone phase did not succeed; failing block");
            return Err(err.into());
        }
        let output = EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type: alloy_consensus::TxType::Legacy,
        };
        self.commit_system_transaction(output, signed_gas_limit, 0, signed_gas_limit)?;

        self.system_tx_phase_cursor = SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            tx_hash: cached_tx_hash,
            receipt_index: 0,
        };
        Ok(())
    }

    fn resolve_phase1_witness(
        &self,
        calldata: &Bytes,
        proposer: Address,
        block_number: u64,
    ) -> Result<Option<(B256, u64)>, BlockExecutionError> {
        let chain_id = self.inner.evm.chain_id();
        let validate_witness = |tx: &TransactionSigned, source: &'static str| {
            validate_phase1_witness_against(tx, calldata.as_ref(), proposer, chain_id, block_number)
                .map_err(|error| {
                    BlockExecutionError::from(
                        ExecutorValidationError::phase1_commit_invalid_witness(source, error),
                    )
                })
        };
        let witness = if let Some(prebuilt) = &self.prebuilt_phase1_tx {
            let tx_hash = validate_witness(prebuilt.tx(), "prebuilt")?;
            (tx_hash, prebuilt.tx().gas_limit())
        } else if let Some(expected) = self.expected_begin_system_txs.first() {
            let tx_hash = validate_witness(expected.tx(), "body[0]")?;
            (tx_hash, expected.tx().gas_limit())
        } else if let Some(signer) = &self.evm_signer {
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
}
