//! Pre-end admission and the terminal call/cleanup execution phase.

use super::*;
use crate::errors::ExecutorValidationError;
use crate::factory::SystemCallGasBudget;
use outbe_primitives::system_tx::{
    build_unsigned_system_tx_with_gas_limit, system_tx_intrinsic_gas, BlockGasBudget, SystemTx,
    SystemTxEnvelopeInput, SystemTxPhase, CALL_EVENT_PROCESSING_ORDINAL,
    CLEANUP_PROCESSING_ORDINAL,
};
use reth_primitives_traits::Recovered;

struct AdmittedEndTx {
    input: SystemTx,
    ordinal: u8,
    gas_limit: u64,
}

struct EndTxCheck {
    block_number: u64,
    capacity: u64,
    call_gas_used: Option<u64>,
    chain_id: u64,
    proposer: Address,
}

fn admit_end_tx(
    tx: &TransactionSigned,
    signer: Address,
    check: EndTxCheck,
) -> Result<AdmittedEndTx, BlockExecutionError> {
    if check.block_number < 2 {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let input = SystemTx::decode(tx.input().as_ref())
        .map_err(|error| ExecutorValidationError::decode_system_tx_at_body_index(0, error))?;
    if input.body_zone() != outbe_primitives::system_tx::BodyZone::EndBlock {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let ordinal = input
        .nonce_ordinal()
        .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
    let gas_limit = expected_end_gas_limit(
        &input,
        ordinal,
        check.capacity,
        check.call_gas_used,
        tx.gas_limit(),
    )?;
    if tx.gas_limit() != gas_limit {
        return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
            kind: input.name(),
            expected: gas_limit,
            actual: tx.gas_limit(),
            capacity: check.capacity,
        }
        .into());
    }
    if signer != check.proposer {
        return Err(ExecutorValidationError::SystemTxSignerMismatch {
            body_index: usize::from(ordinal),
            expected_phase: input.name(),
            proposer: check.proposer,
            signer,
        }
        .into());
    }
    let unsigned = build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
        ordinal,
        block_number: check.block_number,
        chain_id: check.chain_id,
        calldata: tx.input().clone(),
        gas_limit,
    })
    .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
    if tx.signature_hash() != unsigned.signature_hash() {
        return Err(ExecutorValidationError::SystemTxSignatureHashMismatch {
            body_index: usize::from(ordinal),
            expected_phase: input.name(),
        }
        .into());
    }
    Ok(AdmittedEndTx {
        input,
        ordinal,
        gas_limit,
    })
}

fn expected_end_gas_limit(
    input: &SystemTx,
    ordinal: u8,
    capacity: u64,
    call_gas_used: Option<u64>,
    signed_limit: u64,
) -> Result<u64, BlockExecutionError> {
    let required = match input {
        SystemTx::CallEventProcessing
            if call_gas_used.is_none() && ordinal == CALL_EVENT_PROCESSING_ORDINAL =>
        {
            let encoded = SystemTx::CleanupProcessing
                .encode()
                .map_err(ExecutorValidationError::encode_system_tx_for_visible_gas_plan)?;
            let cleanup_intrinsic = system_tx_intrinsic_gas(&encoded)
                .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
            capacity.checked_sub(cleanup_intrinsic)
        }
        SystemTx::CleanupProcessing
            if ordinal == CLEANUP_PROCESSING_ORDINAL && call_gas_used.is_some() =>
        {
            call_gas_used.and_then(|used| capacity.checked_sub(used))
        }
        SystemTx::CallEventProcessing | SystemTx::CleanupProcessing => {
            return Err(ExecutorValidationError::SystemTxPhaseMismatch {
                body_index: usize::from(ordinal),
                expected_phase: input.name(),
                actual_phase: input.name(),
            }
            .into());
        }
        _ => {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
    };
    required.ok_or_else(|| {
        ExecutorValidationError::EndBlockGasLimitMismatch {
            kind: input.name(),
            expected: 0,
            actual: signed_limit,
            capacity,
        }
        .into()
    })
}

impl<E: Evm> OutbeBlockExecutor<'_, E> {
    fn block_gas_budget(&self) -> Result<BlockGasBudget, BlockExecutionError> {
        BlockGasBudget::new(self.inner.evm.block().gas_limit())
            .map_err(|error| ExecutorValidationError::from(error).into())
    }

    fn accounted_block_gas(&self) -> u64 {
        if self.inner.evm.cfg_env().enable_amsterdam_eip8037 {
            self.inner.max_block_gas_used()
        } else {
            self.inner.cumulative_tx_gas_used
        }
    }

    pub fn gas_accounting(&self) -> (u64, u64, u64) {
        (
            self.inner.cumulative_tx_gas_used,
            self.inner.block_regular_gas_used,
            self.inner.block_state_gas_used,
        )
    }

    pub fn tx_gas_limit_cap(&self) -> u64
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        use revm::context_interface::Cfg as _;
        self.inner.evm.cfg_env().tx_gas_limit_cap()
    }

    /// Builder stage after user transactions and before execution finalization.
    /// Repeated calls retain the captured capacity and do not execute the suffix twice.
    pub fn apply_end_block_phase<DB>(&mut self) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + SystemCallGasBudget,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        if matches!(self.system_tx_phase_cursor, SystemTxPhase::EndBlock { .. }) {
            return Ok(());
        }
        let capacity = self
            .block_gas_budget()?
            .end_capacity(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        if block_number >= 2 {
            self.execute_built_end_suffix(capacity)?;
        }
        self.system_tx_phase_cursor = SystemTxPhase::EndBlock {
            gas_capacity: capacity,
        };
        Ok(())
    }

    fn captured_end_capacity(&mut self) -> Result<u64, BlockExecutionError> {
        if let Some(capacity) = self.end_capacity {
            return Ok(capacity);
        }
        let capacity = self
            .block_gas_budget()?
            .end_capacity(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        self.end_capacity = Some(capacity);
        Ok(capacity)
    }

    fn end_block_proposer(&self, block_number: u64) -> Result<Address, BlockExecutionError> {
        if block_number == 0 {
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

    pub(crate) fn take_staged_end_block_txs(&mut self) -> Vec<Recovered<TransactionSigned>> {
        std::mem::take(&mut self.staged_end_block_txs)
    }

    fn execute_built_end_suffix<DB>(&mut self, capacity: u64) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + SystemCallGasBudget,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        let cleanup_intrinsic = system_tx_intrinsic_gas(
            &SystemTx::CleanupProcessing
                .encode()
                .map_err(ExecutorValidationError::encode_system_tx_for_visible_gas_plan)?,
        )
        .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
        let call_limit = capacity.checked_sub(cleanup_intrinsic).ok_or(
            ExecutorValidationError::EndBlockGasLimitMismatch {
                kind: "CallEventProcessing",
                expected: 0,
                actual: capacity,
                capacity,
            },
        )?;
        let call_tx = self.sign_end_tx(SystemTx::CallEventProcessing, call_limit)?;
        self.execute_end_block_system_tx(call_tx.tx(), Address::from(*call_tx.signer()))?;
        let call_gas_used = self
            .end_call_gas_used
            .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
        let cleanup_limit = capacity.checked_sub(call_gas_used).ok_or(
            ExecutorValidationError::EndBlockGasLimitMismatch {
                kind: "CleanupProcessing",
                expected: 0,
                actual: call_gas_used,
                capacity,
            },
        )?;
        let cleanup_tx = self.sign_end_tx(SystemTx::CleanupProcessing, cleanup_limit)?;
        self.execute_end_block_system_tx(cleanup_tx.tx(), Address::from(*cleanup_tx.signer()))?;
        self.staged_end_block_txs = vec![call_tx, cleanup_tx];
        Ok(())
    }

    fn sign_end_tx<DB>(
        &self,
        input: SystemTx,
        gas_limit: u64,
    ) -> Result<Recovered<TransactionSigned>, BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
    {
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let proposer = self.end_block_proposer(block_number)?;
        let evm_signer = self
            .evm_signer
            .as_ref()
            .ok_or(ExecutorValidationError::MissingBeginZoneProposerAddress)?;
        if evm_signer.address() != proposer {
            return Err(ExecutorValidationError::SystemTxSignerMismatch {
                body_index: usize::from(input.nonce_ordinal().unwrap_or(0)),
                expected_phase: input.name(),
                proposer,
                signer: evm_signer.address(),
            }
            .into());
        }
        let ordinal = input
            .nonce_ordinal()
            .ok_or(ExecutorValidationError::MissingBeginZoneProposerAddress)?;
        let calldata = input
            .encode()
            .map_err(ExecutorValidationError::encode_system_tx_for_visible_gas_plan)?;
        let unsigned = build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
            ordinal,
            block_number,
            chain_id: self.chain_id,
            calldata,
            gas_limit,
        })
        .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
        let signed = evm_signer.sign_unsigned(unsigned).map_err(|error| {
            BlockExecutionError::other(
                crate::errors::ExecutorValidationError::plan_visible_system_tx_gas(error),
            )
        })?;
        Ok(Recovered::new_unchecked(signed, proposer))
    }

    pub(in crate::executor) fn execute_end_block_system_tx<DB>(
        &mut self,
        tx: &TransactionSigned,
        signer: Address,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + SystemCallGasBudget,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let capacity = self.captured_end_capacity()?;
        let proposer = self.end_block_proposer(block_number)?;
        let admitted = admit_end_tx(
            tx,
            signer,
            EndTxCheck {
                block_number,
                capacity,
                call_gas_used: self.end_call_gas_used,
                chain_id: self.chain_id,
                proposer,
            },
        )?;
        let input = admitted.input;
        let ordinal = admitted.ordinal;
        let expected_limit = admitted.gas_limit;
        self.inner
            .evm
            .set_end_block_gas_remaining(Some(expected_limit));
        let transact_outcome = self.inner.evm.transact_system_call(
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            tx.input().clone(),
        );
        let charged = match transact_outcome {
            Ok(result) if result.result.is_success() => {
                let output = EthTxResult {
                    result,
                    blob_gas_used: 0,
                    tx_type: tx.tx_type(),
                };
                self.inner.commit_transaction(output).tx_gas_used()
            }
            Ok(result) => {
                let code = super::system_execution::execute::system_tx_failure_code_for_result(
                    &result.result,
                );
                let reason = format!(
                    "system tx {} did not succeed at body_index={}: {:?}",
                    input.name(),
                    ordinal,
                    result.result
                );
                let gas_used = result.result.tx_gas_used();
                drop(result);
                self.commit_end_block_failure_receipt(tx, code, reason, gas_used)
            }
            Err(error) => {
                if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_some() {
                    return Err(BlockExecutionError::other(error));
                }
                let reason = format!(
                    "system tx {} execution failed at body_index={ordinal}: {error}",
                    input.name()
                );
                self.commit_end_block_failure_receipt(tx, 299, reason, 0)
            }
        };
        match input {
            SystemTx::CallEventProcessing => self.end_call_gas_used = Some(charged),
            SystemTx::CleanupProcessing => {
                let call_gas = self
                    .end_call_gas_used
                    .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
                if call_gas.saturating_add(charged) > capacity {
                    return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
                        kind: "CleanupProcessing",
                        expected: capacity.saturating_sub(call_gas),
                        actual: charged,
                        capacity,
                    }
                    .into());
                }
                self.system_tx_phase_cursor = SystemTxPhase::EndBlock {
                    gas_capacity: capacity,
                };
            }
            _ => {}
        }
        Ok(Some(GasOutput::new(charged)))
    }

    fn commit_end_block_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        code: u16,
        reason: String,
        gas_used: u64,
    ) -> u64 {
        let log = crate::failure_receipt::build_outbe_failure_log(
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
        );
        self.inner.cumulative_tx_gas_used =
            self.inner.cumulative_tx_gas_used.saturating_add(gas_used);
        self.inner.block_regular_gas_used =
            self.inner.block_regular_gas_used.saturating_add(gas_used);
        self.inner.receipts.push(Receipt {
            tx_type: tx.tx_type(),
            success: false,
            cumulative_gas_used: self.inner.cumulative_tx_gas_used,
            logs: vec![log],
        });
        gas_used
    }

    pub(in crate::executor) fn check_visible_pre_end_gas(
        &self,
        additional: u64,
    ) -> Result<(), BlockExecutionError> {
        let available = self
            .block_gas_budget()?
            .remaining_pre_end_gas(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        check_capacity(additional, additional, available)
    }

    pub(in crate::executor) fn check_user_pre_end_gas(
        &self,
        gas_limit: u64,
    ) -> Result<(), BlockExecutionError>
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        use revm::context_interface::Cfg;
        let budget = self.block_gas_budget()?;
        let amsterdam = self.inner.evm.cfg_env().enable_amsterdam_eip8037;
        let regular_used = if amsterdam {
            self.inner.block_regular_gas_used
        } else {
            self.inner.cumulative_tx_gas_used
        };
        let available = budget
            .remaining_pre_end_gas(regular_used)
            .map_err(ExecutorValidationError::from)?;
        let regular_limit = gas_limit.min(self.inner.evm.cfg_env().tx_gas_limit_cap());
        check_capacity(gas_limit, regular_limit, available)?;
        if amsterdam {
            let available = budget
                .remaining_pre_end_gas(self.inner.block_state_gas_used)
                .map_err(ExecutorValidationError::from)?;
            check_capacity(gas_limit, gas_limit, available)?;
        }
        Ok(())
    }
}

fn check_capacity(declared: u64, required: u64, available: u64) -> Result<(), BlockExecutionError> {
    if required > available {
        return Err(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: declared,
                block_available_gas: available,
            }
            .into(),
        );
    }
    Ok(())
}
