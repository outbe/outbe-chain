//! Executor orchestration for parent finalization accounting and late-finalize credits.

use super::*;
use crate::{
    errors::ExecutorValidationError,
    finalization::{
        late_credits::verify_late_finalize_credits_batches,
        parent_accounting::{
            resolve_accounted_parent_artifact, resolve_phase1_witness, verify_phase1_certificate,
            ParentArtifactLookup, Phase1VerifyInput, Phase1WitnessLookup,
        },
    },
    system_tx::SystemTxPhase,
};

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
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        resolve_accounted_parent_artifact(ParentArtifactLookup {
            provider: self.accounted_parent_artifact_provider.as_deref(),
            hint: self.parent_artifact_hint,
            block_number,
            parent_hash: self.parent_hash,
            metadata,
        })
    }

    /// Resolves and caches the Phase-1 (`CertifiedParentAccounting`) input and
    /// parent execution summary once per block so `verify_phase1_in_preexec`,
    /// `apply_phase1_commit_in_preexec`, and `begin_block_system_tx_inputs`
    /// never duplicate the parent DB lookup.
    pub(in crate::executor) fn ensure_phase1_input(
        &mut self,
    ) -> Result<(SystemTx, AccountedParentArtifact), BlockExecutionError> {
        if let Some(cached) = self.cached_phase1_input.as_ref() {
            return Ok(cached.clone());
        }
        let resolved = self.certified_parent_begin_input(0)?;
        self.cached_phase1_input = Some(resolved.clone());
        Ok(resolved)
    }

    /// Pre-execution verification of the block's late-finalize credits.
    ///
    /// Verifies the inclusion window (`1 <= block_number - fb_number <= K`) and
    /// the BLS aggregate proof once, caching the verified signer indices so
    /// [`crate::finalization::late_credits::run_late_finalize_credits`] does not
    /// repeat the BLS pairing.
    pub(in crate::executor) fn verify_late_finalize_credits_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        self.verified_late_credit_signers = None;
        if !BlockHeightRegime::for_block(block_number).has_parent_finalization_accounting() {
            return Ok(());
        }
        let Some(artifact) = block_artifacts.late_finalize_credits.as_ref() else {
            return Ok(());
        };
        if artifact.batches.is_empty() {
            return Ok(());
        }

        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let ctx = self.block_context(block_number, timestamp, proposer);
        let verified_batches =
            verify_late_finalize_credits_batches(self.inner.evm.db_mut(), ctx, artifact)?;
        self.verified_late_credit_signers = Some(Arc::new(verified_batches));
        Ok(())
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

        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let block_context = self.block_context(block_number, timestamp, proposer);
        let prevrandao = self.inner.evm.block().prevrandao();
        let parent_hash = self.parent_hash;

        let vrf_proof_hash = verify_phase1_certificate(
            self.inner.evm.db_mut(),
            Phase1VerifyInput {
                block_context,
                parent_hash,
                prevrandao,
                metadata,
            },
        )?;
        self.verified_phase1_vrf_proof_hash = Some(vrf_proof_hash);
        Ok(())
    }

    /// Executes and commits the Phase 1 (`CertifiedParentAccounting`) system tx
    /// in pre-execution so subsequent `BeginBlock` system transactions observe post-Phase-1 state.
    pub(crate) fn apply_phase1_commit_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        if !BlockHeightRegime::for_block(block_number).has_parent_finalization_accounting() {
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

        let Some((cached_tx_hash, _signed_gas_limit)) =
            resolve_phase1_witness(Phase1WitnessLookup {
                prebuilt_phase1_tx: self.prebuilt_phase1_tx.as_ref(),
                expected_begin_system_txs: &self.expected_begin_system_txs,
                evm_signer: self.evm_signer.as_ref(),
                calldata: &calldata,
                proposer,
                chain_id: self.inner.evm.chain_id(),
                block_number,
            })?
        else {
            return Ok(());
        };

        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary: Some(finalized_summary),
            allow_boundary_proposer: self.boundary_allows_proposer(block_artifacts, proposer),
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
            verified_late_credit_signers: None,
            block_gas_limit: self.inner.evm.block().gas_limit(),
            user_gas_used: 0,
        };
        let output = self.execute_preexec_phase1_call(phase_context, calldata)?;
        self.commit_system_transaction(output)?;
        self.system_tx_phase_cursor = SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            tx_hash: cached_tx_hash,
            receipt_index: 0,
        };
        Ok(())
    }

    fn execute_preexec_phase1_call(
        &mut self,
        phase_context: PreloadedSystemTxContext,
        calldata: Bytes,
    ) -> Result<EthTxResult<E::HaltReason, reth_ethereum::TxType>, BlockExecutionError> {
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
        Ok(EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type: alloy_consensus::TxType::Legacy,
        })
    }
}
