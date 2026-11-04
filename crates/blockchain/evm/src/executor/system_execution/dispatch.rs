//! Reserved begin-zone system transactions.

use super::super::*;

struct ValidatedReservedSystemTx {
    body_index: usize,
    input: SystemTx,
    finalized_summary: Option<AccountedParentArtifact>,
    visible_base_gas: u64,
    visible_gas_limit: u64,
    proposer: Address,
}

struct ReservedSystemReceiptContext {
    body_index: usize,
    input: SystemTx,
    block_number: u64,
    visible_base_gas: u64,
    visible_gas_limit: u64,
    available_visible_gas: u64,
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
    pub(in crate::executor) fn execute_reserved_system_tx<R, F>(
        &mut self,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        use crate::errors::ExecutorValidationError;

        outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let tx = recovered.tx();
        let signer = *recovered.signer();
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_artifacts = self.ensure_decoded_block_artifacts()?;

        if self.consume_preexecuted_phase1_witness(tx, &block_artifacts)? {
            return Ok(None);
        }

        let ValidatedReservedSystemTx {
            body_index,
            input,
            finalized_summary,
            visible_base_gas,
            visible_gas_limit,
            proposer,
        } = self.validate_reserved_system_tx((tx, signer), block_number, &block_artifacts)?;

        if matches!(input, SystemTx::HookEvents) {
            return self.commit_reserved_hook_receipt(tx, &block_artifacts, visible_base_gas);
        }

        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary,
            allow_boundary_proposer: self.boundary_allows_proposer(&block_artifacts, proposer),
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
            verified_late_credit_signers: self.verified_late_credit_signers.clone(),
        };
        let available_visible_gas =
            reserved_visible_headroom(body_index, (visible_base_gas, visible_gas_limit))?;
        let output =
            self.invoke_reserved_system_call(tx, body_index, input.name(), phase_context)?;
        let receipt_context = ReservedSystemReceiptContext {
            body_index,
            input,
            block_number,
            visible_base_gas,
            visible_gas_limit,
            available_visible_gas,
        };
        if !output.result.result.is_success() {
            if !f(&output).should_commit() {
                self.check_no_witness_fault()?;
                return Ok(None);
            }
            self.check_no_witness_fault()?;
            return self.commit_reserved_failure_receipt(
                tx,
                &output.result.result,
                receipt_context,
                &block_artifacts,
            );
        }
        self.commit_reserved_success_receipt(output, receipt_context, &block_artifacts, f)
    }

    fn ensure_decoded_block_artifacts(
        &mut self,
    ) -> Result<outbe_primitives::reshare_artifact::OutbeBlockArtifacts, BlockExecutionError> {
        if let Some(artifacts) = self.decoded_block_artifacts.clone() {
            return Ok(artifacts);
        }
        let artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(crate::errors::ExecutorValidationError::block_artifacts)?;
        self.decoded_block_artifacts = Some(artifacts.clone());
        Ok(artifacts)
    }

    fn invoke_reserved_system_call(
        &mut self,
        tx: &TransactionSigned,
        body_index: usize,
        expected_phase: &'static str,
        phase_context: PreloadedSystemTxContext,
    ) -> Result<EthTxResult<E::HaltReason, reth_ethereum::TxType>, BlockExecutionError> {
        let transact_outcome = with_preloaded_system_tx_context(phase_context, || {
            self.inner.evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                tx.input().clone(),
            )
        });
        let result = transact_outcome.map_err(|error| {
            let reason = format!(
                "system tx {expected_phase} execution failed at body_index={body_index}: {error}"
            );
            if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_none() {
                tracing::error!(target: "outbe::executor", %reason);
            }
            BlockExecutionError::other(error)
        })?;
        Ok(EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type: tx.tx_type(),
        })
    }

    fn commit_reserved_hook_receipt(
        &mut self,
        tx: &TransactionSigned,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
        visible_base_gas: u64,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let logs = std::mem::take(&mut self.whitelisted_hook_event_logs);
        let commit_outcome = self
            .push_hook_events_receipt(tx.tx_type(), logs, visible_base_gas)
            .map(Some);
        if commit_outcome.is_ok() {
            self.advance_system_tx_phase_cursor(block_artifacts);
        }
        commit_outcome
    }

    fn commit_reserved_success_receipt<F>(
        &mut self,
        output: EthTxResult<E::HaltReason, reth_ethereum::TxType>,
        context: ReservedSystemReceiptContext,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let ReservedSystemReceiptContext {
            visible_base_gas,
            visible_gas_limit,
            ..
        } = context;
        if !f(&output).should_commit() {
            return Ok(None);
        }
        let commit_outcome = self
            .commit_system_transaction(output, visible_base_gas, 0, visible_gas_limit)
            .map(Some);
        if commit_outcome.is_ok() {
            self.advance_system_tx_phase_cursor(block_artifacts);
        }
        commit_outcome
    }

    fn commit_reserved_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        result: &ExecutionResult<HaltReason>,
        context: ReservedSystemReceiptContext,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let ReservedSystemReceiptContext {
            body_index,
            input,
            block_number,
            visible_base_gas,
            visible_gas_limit,
            available_visible_gas,
        } = context;
        let expected_phase = input.name();
        tracing::error!(
            target: "outbe::executor",
            expected_phase,
            body_index,
            block_number,
            gas_used = result.tx_gas_used(),
            gas_limit = tx.gas_limit(),
            result = ?result,
            "system tx failed"
        );
        let code = system_tx_failure_code_for_result(result);
        if input.revert_fails_block() {
            let err = crate::errors::ExecutorValidationError::critical_system_tx_did_not_succeed(
                expected_phase,
                body_index,
                block_number,
                code,
                result,
            );
            tracing::error!(target: "outbe::executor", %err, "critical begin-zone phase did not succeed; failing block");
            return Err(err.into());
        }
        let reason = format!(
            "system tx {expected_phase} did not succeed at body_index={body_index}: {:?}",
            result
        );
        let additional_visible_gas = if matches!(
            result,
            ExecutionResult::Halt {
                reason: HaltReason::OutOfGas(_),
                ..
            }
        ) {
            available_visible_gas
        } else {
            0
        };
        let gas_output = self.push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: tx.tx_type(),
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
            visible_base_gas,
            additional_visible_gas,
            signed_gas_limit: visible_gas_limit,
            internal_gas_used: result.tx_gas_used(),
        })?;
        self.advance_system_tx_phase_cursor(block_artifacts);
        Ok(Some(gas_output))
    }

    fn consume_preexecuted_phase1_witness(
        &mut self,
        tx: &TransactionSigned,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<bool, BlockExecutionError> {
        if let crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            tx_hash: cached_hash,
            ..
        } = self.system_tx_phase_cursor
        {
            if !cached_hash.is_zero() {
                if tx.signature_hash() != cached_hash {
                    return Err(
                        crate::errors::ExecutorValidationError::Phase1WitnessSignatureHashMismatch {
                            expected: cached_hash,
                            actual: tx.signature_hash(),
                        }
                        .into(),
                    );
                }
                self.advance_system_tx_phase_cursor(block_artifacts);
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn validate_reserved_system_tx(
        &mut self,
        transaction: (&TransactionSigned, Address),
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ValidatedReservedSystemTx, BlockExecutionError> {
        let (tx, signer) = transaction;
        let (body_index, expected_input, finalized_summary, visible_base_gas, planned_gas_limit) =
            self.expected_system_tx_for_cursor(block_number, block_artifacts)?;
        validate_reserved_system_input(tx, body_index, &expected_input)?;

        self.validate_reserved_system_envelope(
            tx,
            (body_index, expected_input.name(), planned_gas_limit),
            block_number,
        )?;
        let visible_gas_limit = tx.gas_limit();

        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        if signer != proposer {
            return Err(
                crate::errors::ExecutorValidationError::SystemTxSignerMismatch {
                    body_index,
                    expected_phase: expected_input.name(),
                    proposer,
                    signer,
                }
                .into(),
            );
        }

        Ok(ValidatedReservedSystemTx {
            body_index,
            input: expected_input,
            finalized_summary,
            visible_base_gas,
            visible_gas_limit,
            proposer,
        })
    }

    fn validate_reserved_system_envelope(
        &self,
        tx: &TransactionSigned,
        expected: (usize, &'static str, u64),
        block_number: u64,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let (body_index, expected_phase, planned_gas_limit) = expected;
        let ordinal = body_index.try_into().map_err(|_| -> BlockExecutionError {
            ExecutorValidationError::SystemTxBodyIndexExceedsU8 { body_index }.into()
        })?;
        let unsigned = build_unsigned_system_tx_with_gas_limit(
            outbe_primitives::system_tx::SystemTxEnvelopeInput {
                ordinal,
                block_number,
                chain_id: self.inner.evm.chain_id(),
                calldata: tx.input().clone(),
                gas_limit: planned_gas_limit,
            },
        )
        .map_err(|error| -> BlockExecutionError {
            ExecutorValidationError::build_expected_system_tx(body_index, error).into()
        })?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(ExecutorValidationError::SystemTxSignatureHashMismatch {
                body_index,
                expected_phase,
            }
            .into());
        }
        Ok(())
    }
}

fn validate_reserved_system_input(
    tx: &TransactionSigned,
    body_index: usize,
    expected_input: &SystemTx,
) -> Result<(), BlockExecutionError> {
    use crate::errors::ExecutorValidationError;

    let expected_phase = expected_input.name();
    let actual_input =
        SystemTx::decode(tx.input().as_ref()).map_err(|error| -> BlockExecutionError {
            ExecutorValidationError::decode_system_tx_at_body_index(body_index, error).into()
        })?;
    let actual_phase = actual_input.name();
    if actual_input.selector() != expected_input.selector() {
        return Err(ExecutorValidationError::SystemTxPhaseMismatch {
            body_index,
            expected_phase,
            actual_phase,
        }
        .into());
    }
    if &actual_input != expected_input {
        return Err(ExecutorValidationError::SystemTxCalldataMismatch {
            body_index,
            expected_phase,
        }
        .into());
    }

    Ok(())
}

fn reserved_visible_headroom(
    body_index: usize,
    visible_gas: (u64, u64),
) -> Result<u64, BlockExecutionError> {
    let (visible_base_gas, visible_gas_limit) = visible_gas;
    visible_gas_limit
        .checked_sub(visible_base_gas)
        .ok_or_else(|| {
            crate::errors::ExecutorValidationError::SystemTxSignedGasBelowVisibleBase {
                body_index,
                signed: visible_gas_limit,
                visible_base: visible_base_gas,
            }
            .into()
        })
}
