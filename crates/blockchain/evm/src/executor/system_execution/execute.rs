use super::super::*;

/// Maps an `ExecutionResult` to a stable `u16` `OutbeFailure` code. The result
/// comes from a Phase 1-4 system tx that produced `!is_success`. The code is in
/// the 200-299 band reserved for `outbe-evm` phase failures. The exhaustive
/// `match` makes adding a new revm variant a compile error.
///
/// Codes:
/// - 201 - explicit revert (Solidity `require`, `revert`, etc.)
/// - 202 - out-of-gas (any `OutOfGasError` variant)
/// - 299 - other halt reasons (precompile error, opcode not found, ...)
pub(crate) fn system_tx_failure_code_for_result(result: &ExecutionResult<HaltReason>) -> u16 {
    match result {
        // Callers only reach this fn under `!result.is_success()`, so the Success
        // arm is unreachable in practice. Map it deterministically to the generic
        // 299 fallback instead of a `debug_assert!` panic. The executor path uses
        // no panic-class macro.
        ExecutionResult::Success { .. } => 299,
        ExecutionResult::Revert { .. } => 201,
        ExecutionResult::Halt { reason, .. } => match reason {
            HaltReason::OutOfGas(OutOfGasError::Basic)
            | HaltReason::OutOfGas(OutOfGasError::MemoryLimit)
            | HaltReason::OutOfGas(OutOfGasError::Memory)
            | HaltReason::OutOfGas(OutOfGasError::Precompile)
            | HaltReason::OutOfGas(OutOfGasError::InvalidOperand)
            | HaltReason::OutOfGas(OutOfGasError::ReentrancySentry) => 202,
            _ => 299,
        },
    }
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
    /// Commits an Outbe begin-zone system transaction under the Tempo 0-gas model.
    ///
    /// The precompile executes under the separate internal system-work budget
    /// (`SYSTEM_TX_ARTIFACT_GAS_LIMIT`, 10B gas). The public Ethereum block gas
    /// counters (`cumulative_tx_gas_used`, `block_regular_gas_used`,
    /// `block_state_gas_used`) remain unchanged (`0` visible gas).
    pub(crate) fn commit_system_transaction(
        &mut self,
        output: EthTxResult<E::HaltReason, alloy_consensus::TxType>,
    ) -> Result<GasOutput, BlockExecutionError> {
        let user_cumulative_tx_gas = self.inner.cumulative_tx_gas_used;
        let user_regular_gas = self.inner.block_regular_gas_used;
        let user_state_gas = self.inner.block_state_gas_used;
        let system_cumulative_gas_used =
            self.checked_system_tx_execution_gas(output.result.result.tx_gas_used())?;
        let system_cumulative_gross_gas = self
            .system_tx_gross_gas
            .checked_add(output.result.result.gas().total_gas_spent())
            .ok_or(crate::errors::ExecutorValidationError::InternalSystemWorkGasOverflow)?;

        let _ = self.inner.commit_transaction(output);
        self.system_tx_execution_gas = system_cumulative_gas_used;
        self.system_tx_gross_gas = system_cumulative_gross_gas;

        if let Some(receipt) = self.inner.receipts.last_mut() {
            receipt.cumulative_gas_used = user_cumulative_tx_gas;
        }

        self.inner.cumulative_tx_gas_used = user_cumulative_tx_gas;
        self.inner.block_regular_gas_used = user_regular_gas;
        self.inner.block_state_gas_used = user_state_gas;

        Ok(GasOutput::new(0))
    }

    pub(in crate::executor) fn checked_system_tx_execution_gas(
        &self,
        additional: u64,
    ) -> Result<u64, BlockExecutionError> {
        let cumulative = self
            .system_tx_execution_gas
            .checked_add(additional)
            .ok_or(crate::errors::ExecutorValidationError::InternalSystemWorkGasOverflow)?;
        let limit = outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;
        if cumulative > limit {
            return Err(
                crate::errors::ExecutorValidationError::InternalSystemWorkBudgetExceeded {
                    cumulative,
                    limit,
                    additional,
                }
                .into(),
            );
        }
        Ok(cumulative)
    }
}
