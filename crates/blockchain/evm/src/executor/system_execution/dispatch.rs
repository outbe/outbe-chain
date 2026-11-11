//! Reserved begin-zone system transactions.

use super::super::*;

struct ValidatedReservedSystemTx {
    body_index: usize,
    input: SystemTx,
    finalized_summary: Option<AccountedParentArtifact>,
    proposer: Address,
}

struct ReservedSystemReceiptContext {
    body_index: usize,
    input: SystemTx,
    block_number: u64,
}

impl<E> OutbeBlockExecutor<'_, E> {
    pub fn advance_system_tx_step(&mut self, input: &SystemTx) -> Result<(), BlockExecutionError> {
        self.fsm
            .on_system_tx(input)
            .map_err(crate::errors::ExecutorValidationError::from)?;
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
    pub fn execute_reserved_system_tx<R, F>(
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

        if self.consume_preexecuted_phase1_witness(tx)? {
            return Ok(None);
        }

        let ValidatedReservedSystemTx {
            body_index,
            input,
            finalized_summary,
            proposer,
        } = self.validate_reserved_system_tx((tx, signer), block_number, &block_artifacts)?;

        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary,
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
            verified_late_credit_signers: self.verified_late_credit_signers.clone(),
            block_gas_limit: self.inner.evm.block().gas_limit(),
            user_gas_used: 0,
        };
        let output = self.invoke_reserved_system_call(
            tx,
            body_index,
            self.block_order.step_name_or_stage(&input),
            phase_context,
        )?;
        let receipt_context = ReservedSystemReceiptContext {
            body_index,
            input,
            block_number,
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
            );
        }
        self.commit_reserved_success_receipt(output, &receipt_context.input, f)
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

    fn commit_reserved_success_receipt<F>(
        &mut self,
        output: EthTxResult<E::HaltReason, reth_ethereum::TxType>,
        input: &SystemTx,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        if !f(&output).should_commit() {
            return Ok(None);
        }
        let gas_output = self.commit_system_transaction(output)?;
        self.advance_system_tx_step(input)?;
        Ok(Some(gas_output))
    }

    fn commit_reserved_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        result: &ExecutionResult<HaltReason>,
        context: ReservedSystemReceiptContext,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let ReservedSystemReceiptContext {
            body_index,
            input,
            block_number,
        } = context;
        let expected_phase = self.block_order.step_name_or_stage(&input);
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
        if self
            .block_order
            .step_for_tx(&input)
            .is_none_or(|step| step.revert_fails_block)
        {
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
        let gas_output = self.push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type: tx.tx_type(),
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
            internal_gas_used: result.tx_gas_used(),
            internal_gross_gas_used: result.gas().total_gas_spent(),
        })?;
        self.advance_system_tx_step(&input)?;
        Ok(Some(gas_output))
    }

    fn consume_preexecuted_phase1_witness(
        &mut self,
        tx: &TransactionSigned,
    ) -> Result<bool, BlockExecutionError> {
        let Some(cached_hash) = self.preexecuted_phase1_tx_hash else {
            return Ok(false);
        };
        if tx.signature_hash() != cached_hash {
            return Err(
                crate::errors::ExecutorValidationError::Phase1WitnessSignatureHashMismatch {
                    expected: cached_hash,
                    actual: tx.signature_hash(),
                }
                .into(),
            );
        }
        let input =
            SystemTx::decode(tx.input()).map_err(crate::errors::ExecutorValidationError::from)?;
        self.advance_system_tx_step(&input)?;
        self.preexecuted_phase1_tx_hash = None;
        Ok(true)
    }

    fn validate_reserved_system_tx(
        &mut self,
        transaction: (&TransactionSigned, Address),
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ValidatedReservedSystemTx, BlockExecutionError> {
        let (tx, signer) = transaction;
        let (body_index, expected_input, finalized_summary) =
            self.expected_system_tx_for_cursor(block_number, block_artifacts)?;
        validate_reserved_system_input(tx, body_index, &expected_input, &self.block_order)?;

        self.validate_reserved_system_envelope(
            tx,
            (
                body_index,
                self.block_order.step_name_or_stage(&expected_input),
            ),
            block_number,
        )?;

        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        if signer != proposer {
            return Err(
                crate::errors::ExecutorValidationError::SystemTxSignerMismatch {
                    body_index,
                    expected_phase: self.block_order.step_name_or_stage(&expected_input),
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
            proposer,
        })
    }

    fn validate_reserved_system_envelope(
        &self,
        tx: &TransactionSigned,
        expected: (usize, &'static str),
        block_number: u64,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let (body_index, expected_phase) = expected;
        let ordinal = body_index.try_into().map_err(|_| -> BlockExecutionError {
            ExecutorValidationError::SystemTxBodyIndexExceedsU8 { body_index }.into()
        })?;
        let unsigned = build_unsigned_system_tx(
            ordinal,
            block_number,
            self.inner.evm.chain_id(),
            tx.input().clone(),
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
    order: &crate::system_tx::BlockOrder,
) -> Result<(), BlockExecutionError> {
    use crate::errors::ExecutorValidationError;

    let expected_phase = order.step_name_or_stage(expected_input);
    let actual_input =
        SystemTx::decode(tx.input().as_ref()).map_err(|error| -> BlockExecutionError {
            ExecutorValidationError::decode_system_tx_at_body_index(body_index, error).into()
        })?;
    let actual_phase = order.step_name_or_stage(&actual_input);
    if actual_input.step_id() != expected_input.step_id() {
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
