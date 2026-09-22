use super::super::*;

/// Maps an `ExecutionResult` (from a Phase 1-4 system tx that produced
/// `!is_success`) to a stable `u16` `OutbeFailure` code in the 200-299
/// band reserved for `outbe-evm` phase failures. The exhaustive `match`
/// makes adding a new revm variant a compile error.
///
/// Codes:
/// - 201 - explicit revert (Solidity `require`, `revert`, etc.)
/// - 202 - out-of-gas (any `OutOfGasError` variant)
/// - 299 - other halt reasons (precompile error, opcode not found, ...)
pub(crate) fn system_tx_failure_code_for_result(result: &ExecutionResult<HaltReason>) -> u16 {
    match result {
        // Callers only reach this fn under `!result.is_success()`, so the Success
        // arm is unreachable in practice; map it to the generic 299 fallback
        // deterministically rather than `debug_assert!`-panicking (no panic-class
        // macro on the executor path).
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

pub(in crate::executor) fn is_ocomp_deadline_passed_revert(
    result: &ExecutionResult<HaltReason>,
) -> bool {
    matches!(
        result,
        ExecutionResult::Revert { output, .. }
            if outbe_metadosis::is_deadline_passed_result_vote_revert_data(output.as_ref())
    )
}

pub(in crate::executor) fn is_nod_materialization_soft_revert(
    result: &ExecutionResult<HaltReason>,
) -> bool {
    matches!(
        result,
        ExecutionResult::Revert { output, .. }
            if outbe_nodfactory::materialization::is_soft_materialization_revert_data(
                output.as_ref()
            )
    )
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
    /// Commits an Outbe begin-zone system transaction with separate internal
    /// and visible gas accounting.
    ///
    /// The precompile executes under the separate internal system-work budget.
    /// The public Ethereum block gas lane charges the signed envelope's visible
    /// base gas (intrinsic plus any schedule-hashed protocol precharge) and any
    /// explicit compressed-entity gas, without exposing the internal execution
    /// lane.
    pub(in crate::executor) fn commit_system_transaction(
        &mut self,
        output: EthTxResult<E::HaltReason, alloy_consensus::TxType>,
        visible_base_gas: u64,
        compressed_entities_gas: u64,
        signed_gas_limit: u64,
    ) -> Result<GasOutput, BlockExecutionError> {
        let visible_gas_used = self.visible_system_gas_with_compressed_entities(
            visible_base_gas,
            compressed_entities_gas,
            signed_gas_limit,
        )?;
        let user_cumulative_tx_gas = self.inner.cumulative_tx_gas_used;
        let user_regular_gas = self.inner.block_regular_gas_used;
        let user_state_gas = self.inner.block_state_gas_used;
        let visible_cumulative_tx_gas = user_cumulative_tx_gas
            .checked_add(visible_gas_used)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "system tx visible gas overflow".into(),
                ))
            })?;
        let system_cumulative_gas_used =
            self.checked_system_tx_execution_gas(output.result.result.tx_gas_used())?;

        let _ = self.inner.commit_transaction(output);
        self.system_tx_execution_gas = system_cumulative_gas_used;

        if let Some(receipt) = self.inner.receipts.last_mut() {
            receipt.cumulative_gas_used = visible_cumulative_tx_gas;
        }

        self.inner.cumulative_tx_gas_used = visible_cumulative_tx_gas;
        self.inner.block_regular_gas_used = user_regular_gas.saturating_add(visible_gas_used);
        self.inner.block_state_gas_used = user_state_gas.saturating_add(visible_gas_used);

        Ok(GasOutput::new(visible_gas_used))
    }

    pub(in crate::executor) fn checked_system_tx_execution_gas(
        &self,
        additional: u64,
    ) -> Result<u64, BlockExecutionError> {
        let cumulative = self
            .system_tx_execution_gas
            .checked_add(additional)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "internal system-work gas overflow".into(),
                ))
            })?;
        let limit = outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;
        if cumulative > limit {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "internal system-work budget exceeded: cumulative={cumulative}, limit={limit}, additional={additional}"
                    )
                    .into(),
                ),
            ));
        }
        Ok(cumulative)
    }
}
