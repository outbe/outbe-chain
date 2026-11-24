use super::super::*;

/// Maps a [`TxResult`] to a stable `u16` `OutbeFailure` code. The result
/// comes from a Phase 1-4 system tx that produced `!is_success`. The code is in
/// the 200-299 band reserved for `outbe-evm` phase failures.
///
/// Codes:
/// - 201 - explicit revert (Solidity `require`, `revert`, etc.)
/// - 202 - out-of-gas (any OOG stop reason)
/// - 299 - other halt reasons (precompile error, opcode not found, ...)
pub fn system_tx_failure_code_for_result<E>(result: &evm2::TxResultExt<E>) -> u16 {
    if result.is_success() {
        return 299;
    }
    if result.stop.is_revert() {
        return 201;
    }
    if result.stop.is_out_of_gas() {
        return 202;
    }
    match result.stop {
        InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct => 299,
        _ => 299,
    }
}

impl OutbeBlockExecutor<'_> {
    /// Commits an Outbe begin-zone system transaction under the Tempo 0-gas model.
    ///
    /// The precompile executes under the separate internal system-work budget
    /// (`SYSTEM_TX_ARTIFACT_GAS_LIMIT`, 10B gas). The public Ethereum block gas
    /// counters (`cumulative_gas_used`, `block_regular_gas_used`,
    /// `block_state_gas_used`) remain unchanged (`0` visible gas).
    pub fn commit_system_transaction(
        &mut self,
        output: EthTransactionResultWithState<OutbeEvmTypes, alloy_consensus::TxType>,
    ) -> Result<GasOutput, BlockExecutionError> {
        let user_cumulative_tx_gas = self.inner.inner.cumulative_gas_used;
        let user_regular_gas = self.inner.inner.block_regular_gas_used;
        let user_state_gas = self.inner.inner.block_state_gas_used;
        let tx_gas_used = output.result().result.tx_gas_used();
        let gross_gas = output.result().result.total_gas_spent;

        let _ = self.inner.commit_transaction(output)?;
        self.system_tx_execution_gas = self.system_tx_execution_gas.saturating_add(tx_gas_used);
        self.system_tx_gross_gas = self.system_tx_gross_gas.saturating_add(gross_gas);

        if let Some(receipt) = self.inner.inner.receipts.last_mut() {
            receipt.cumulative_gas_used = user_cumulative_tx_gas;
        }

        self.inner.inner.cumulative_gas_used = user_cumulative_tx_gas;
        self.inner.inner.block_regular_gas_used = user_regular_gas;
        self.inner.inner.block_state_gas_used = user_state_gas;

        Ok(GasOutput::new(0, 0))
    }
}
