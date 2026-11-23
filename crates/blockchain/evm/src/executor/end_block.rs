//! Terminal `EndBlock` system transaction execution phase (`END_BLOCK_ORDER`).

use super::*;
use crate::config::system_txs::sign_end_system_tx;
use crate::errors::ExecutorValidationError;
use reth_primitives_traits::Recovered;
use {
    crate::system_tx::PlannedSystemStep,
    outbe_primitives::system_tx::{build_unsigned_system_tx, BlockExecutionStage, SystemTx},
};

struct AdmittedEndTx {
    input: SystemTx,
    ordinal: u8,
}

struct EndTxCheck {
    block_number: u64,
    expected: PlannedSystemStep,
    order: crate::system_tx::BlockOrder,
    chain_id: u64,
    proposer: Address,
}

fn admit_end_tx(
    tx: &TransactionSigned,
    signer: Address,
    check: EndTxCheck,
) -> Result<AdmittedEndTx, BlockExecutionError> {
    let (input, ordinal) = decode_expected_end_tx(tx, &check)?;
    if tx.gas_limit() != 0 {
        return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
            kind: check.order.step_name_or_stage(&input),
            expected: 0,
            actual: tx.gas_limit(),
            capacity: 0,
        }
        .into());
    }
    if signer != check.proposer {
        return Err(ExecutorValidationError::SystemTxSignerMismatch {
            body_index: usize::from(ordinal),
            expected_phase: check.order.step_name_or_stage(&input),
            proposer: check.proposer,
            signer,
        }
        .into());
    }
    let unsigned = build_unsigned_system_tx(
        ordinal,
        check.block_number,
        check.chain_id,
        tx.input().clone(),
    )
    .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
    if tx.signature_hash() != unsigned.signature_hash() {
        return Err(ExecutorValidationError::SystemTxSignatureHashMismatch {
            body_index: usize::from(ordinal),
            expected_phase: check.order.step_name_or_stage(&input),
        }
        .into());
    }
    Ok(AdmittedEndTx { input, ordinal })
}

fn decode_expected_end_tx(
    tx: &TransactionSigned,
    check: &EndTxCheck,
) -> Result<(SystemTx, u8), BlockExecutionError> {
    if !BlockHeightRegime::for_block(check.block_number).has_end_block_system_txs() {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let input = SystemTx::decode(tx.input().as_ref())
        .map_err(|error| ExecutorValidationError::decode_system_tx_at_body_index(0, error))?;
    if input.body_zone() != outbe_primitives::system_tx::BodyZone::EndBlock {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let ordinal = check
        .expected
        .end_nonce_ordinal()
        .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
    if input.step_id() != check.expected.id {
        return Err(ExecutorValidationError::SystemTxPhaseMismatch {
            body_index: usize::from(ordinal),
            expected_phase: check.expected.step.name,
            actual_phase: check.order.step_name_or_stage(&input),
        }
        .into());
    }
    Ok((input, ordinal))
}

impl OutbeBlockExecutor<'_> {
    pub fn accounted_block_gas(&self) -> u64 {
        if self.inner.evm().version().feature(EvmFeatures::EIP8037) {
            self.inner
                .inner
                .block_regular_gas_used
                .max(self.inner.inner.block_state_gas_used)
        } else {
            self.inner.inner.cumulative_gas_used
        }
    }

    pub fn gas_accounting(&self) -> (u64, u64, u64) {
        (
            self.inner.inner.cumulative_gas_used,
            self.inner.inner.block_regular_gas_used,
            self.inner.inner.block_state_gas_used,
        )
    }

    pub fn tx_gas_limit_cap(&self) -> u64 {
        self.inner.evm().version().tx_gas_limit_cap
    }

    /// Builder stage after user transactions and before execution finalization.
    /// Repeated calls are idempotent and do not execute the suffix twice.
    pub fn apply_end_block_phase(&mut self) -> Result<(), BlockExecutionError> {
        let stage = self.enter_end_block_phase_if_needed()?;
        if stage == BlockExecutionStage::EndBlock && self.fsm.end_cursor() == 0 {
            self.execute_built_end_suffix()?;
        }
        Ok(())
    }

    fn enter_end_block_phase_if_needed(
        &mut self,
    ) -> Result<BlockExecutionStage, BlockExecutionError> {
        let block_number = self.block_number();
        self.fsm
            .enter_end_block(block_number)
            .map_err(|error| ExecutorValidationError::from(error).into())
    }

    fn end_block_proposer(&self, block_number: u64) -> Result<Address, BlockExecutionError> {
        if BlockHeightRegime::for_block(block_number).is_genesis() {
            return Err(ExecutorValidationError::MissingBeginZoneProposerAddress.into());
        }
        self.proposer_evm_address
            .or_else(|| {
                self.expected_begin_system_txs
                    .first()
                    .map(|tx| Address::from(*tx.signer()))
            })
            .or_else(|| self.evm_signer.as_ref().map(|signer| signer.address()))
            .ok_or_else(|| ExecutorValidationError::MissingBeginZoneProposerAddress.into())
    }

    pub fn take_staged_end_block_txs(&mut self) -> Vec<Recovered<TransactionSigned>> {
        std::mem::take(&mut self.staged_end_block_txs)
    }

    fn execute_built_end_suffix(&mut self) -> Result<(), BlockExecutionError> {
        let steps = self.fsm.plan().end_block_steps.clone();
        let mut staged = Vec::with_capacity(steps.len());
        for planned in steps {
            let input = SystemTx::EndBlock {
                step_index: planned.id.step_index,
            };
            let signed_tx = self.sign_end_tx(input)?;
            self.execute_end_block_system_tx(signed_tx.tx(), Address::from(*signed_tx.signer()))?;
            staged.push(signed_tx);
        }
        self.staged_end_block_txs = staged;
        Ok(())
    }

    fn sign_end_tx(
        &self,
        input: SystemTx,
    ) -> Result<Recovered<TransactionSigned>, BlockExecutionError> {
        let block_number = self.block_number();
        let proposer = self.end_block_proposer(block_number)?;
        sign_end_system_tx(
            self.evm_signer.as_ref(),
            proposer,
            block_number,
            self.chain_id,
            input,
            &self.block_order,
        )
    }

    pub fn execute_end_block_system_tx(
        &mut self,
        tx: &TransactionSigned,
        signer: Address,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let block_number = self.block_number();
        if !BlockHeightRegime::for_block(block_number).has_end_block_system_txs() {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
        self.enter_end_block_phase_if_needed()?;
        let expected = self
            .fsm
            .expected_step()
            .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
        let proposer = self.end_block_proposer(block_number)?;
        let admitted = admit_end_tx(
            tx,
            signer,
            EndTxCheck {
                block_number,
                expected,
                order: self.block_order,
                chain_id: self.chain_id,
                proposer,
            },
        )?;
        let input = admitted.input;
        let ordinal = admitted.ordinal;
        let block_gas_limit = self.block_gas_limit();
        let user_gas_used = self.accounted_block_gas();
        let tx_hash = *tx.tx_hash();
        let witness_scope = self.prepare_tx_witness_scope(false)?;
        let transact_outcome = with_preloaded_system_tx_context(
            PreloadedSystemTxContext::for_end_block(proposer, block_gas_limit, user_gas_used),
            || {
                transact_outbe_system_call(
                    self.inner.evm_mut(),
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                    outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                    tx.input().clone(),
                )
            },
        );
        drop(witness_scope);
        match transact_outcome {
            Ok(result) if result.result.is_success() => {
                let user_cumulative_tx_gas = self.inner.inner.cumulative_gas_used;
                let user_regular_gas = self.inner.inner.block_regular_gas_used;
                let user_state_gas = self.inner.inner.block_state_gas_used;
                let output = EthTransactionResultWithState::new(result, tx.tx_type(), 0);
                let _ = self.inner.commit_transaction(output)?;
                if let Some(receipt) = self.inner.inner.receipts.last_mut() {
                    receipt.cumulative_gas_used = user_cumulative_tx_gas;
                }
                self.inner.inner.cumulative_gas_used = user_cumulative_tx_gas;
                self.inner.inner.block_regular_gas_used = user_regular_gas;
                self.inner.inner.block_state_gas_used = user_state_gas;
            }
            Ok(result) => {
                let code = super::system_execution::execute::system_tx_failure_code_for_result(
                    &result.result,
                );
                let reason = format!(
                    "system tx {} did not succeed at body_index={}: {:?}",
                    self.block_order.step_name_or_stage(&input),
                    ordinal,
                    result.result
                );
                drop(result);
                self.commit_end_block_failure_receipt(tx, code, reason);
            }
            Err(error) => {
                let reason = format!(
                    "system tx {} execution failed at body_index={ordinal}: {error}",
                    self.block_order.step_name_or_stage(&input)
                );
                if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_none()
                    && outbe_primitives::projection::NodeLocalExecutionAbort::find(&error).is_none()
                {
                    tracing::error!(target: "outbe::executor", %reason);
                }
                return Err(BlockExecutionError::other(error));
            }
        }
        // A reverted step is still in the body, and the verifier repeats its enclave calls.
        self.finish_tx_witness(tx_hash, true)?;
        self.advance_system_tx_step(&input)?;
        Ok(Some(GasOutput::new(0, 0)))
    }

    fn commit_end_block_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        code: u16,
        reason: String,
    ) {
        let log = crate::failure_receipt::build_outbe_failure_log(
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
        );
        self.inner.inner.receipts.push(Receipt {
            tx_type: tx.tx_type(),
            success: false,
            cumulative_gas_used: self.inner.inner.cumulative_gas_used,
            logs: vec![log],
        });
    }
}
