use super::*;

pub(in crate::executor) struct SystemFailureReceiptInput {
    pub(super) tx_type: alloy_consensus::TxType,
    pub(super) log_address: Address,
    pub(super) code: u16,
    pub(super) reason: String,
    pub(super) internal_gas_used: u64,
    pub(super) internal_gross_gas_used: u64,
}

pub(in crate::executor) fn validator_fee_for_gas(
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: Option<u128>,
    gas_used: u64,
    base_fee_per_gas: u128,
) -> U256 {
    let max_priority_fee_per_gas = max_priority_fee_per_gas
        .unwrap_or_else(|| max_fee_per_gas.saturating_sub(base_fee_per_gas));
    let fee_cap_above_base = max_fee_per_gas.saturating_sub(base_fee_per_gas);
    let validator_fee_per_gas = max_priority_fee_per_gas.min(fee_cap_above_base);
    U256::from(validator_fee_per_gas) * U256::from(gas_used)
}

pub(in crate::executor) fn burned_fee_for_gas(
    base_fee_per_gas: u128,
    gas_used: u64,
    blob_gas_price: u128,
    blob_gas_used: u64,
) -> Option<U256> {
    let base_fee_minor = U256::from(base_fee_per_gas).checked_mul(U256::from(gas_used))?;
    let blob_fee_minor = U256::from(blob_gas_price).checked_mul(U256::from(blob_gas_used))?;
    base_fee_minor.checked_add(blob_fee_minor)
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    /// Pushes a `status=0` synthetic receipt with exactly one
    /// `OutbeFailure(code, reason)` log, advances the user transaction gas
    /// accumulators by `SOFT_FAILURE_GAS`, and leaves EVM state untouched.
    ///
    /// Used by:
    /// - the zero-fee user-tx path (`outbe-zerofee` rejection).
    ///
    /// System transaction failures use [`Self::push_system_failure_receipt`]
    /// so they charge the signed envelope's visible gas while keeping internal
    /// execution gas in `system_tx_execution_gas`.
    ///
    /// Determinism: the synthetic log encoding depends only on
    /// `(log_address, code, reason)`. Identical inputs across proposer
    /// and validators yield byte-equal receipts and therefore byte-equal
    /// `receipts_root`. See `crate::failure_receipt`.
    pub(crate) fn push_failure_receipt(
        &mut self,
        tx_type: alloy_consensus::TxType,
        log_address: Address,
        code: u16,
        reason: String,
    ) {
        let log = crate::failure_receipt::build_outbe_failure_log(log_address, code, reason);
        let user_cumulative_gas_used = self
            .inner
            .cumulative_tx_gas_used
            .saturating_add(Self::SOFT_FAILURE_GAS);
        self.inner.receipts.push(Receipt {
            tx_type,
            success: false,
            cumulative_gas_used: user_cumulative_gas_used,
            logs: vec![log],
        });
        self.inner.cumulative_tx_gas_used = user_cumulative_gas_used;
        self.inner.block_regular_gas_used = self
            .inner
            .block_regular_gas_used
            .saturating_add(Self::SOFT_FAILURE_GAS);
        self.inner.block_state_gas_used = self
            .inner
            .block_state_gas_used
            .saturating_add(Self::SOFT_FAILURE_GAS);
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
    /// Pushes a `status=0` system synthetic receipt with `0` visible gas under
    /// the Tempo 0-gas model while accounting internal execution gas in
    /// `system_tx_execution_gas`.
    pub(in crate::executor) fn push_system_failure_receipt(
        &mut self,
        input: SystemFailureReceiptInput,
    ) -> Result<GasOutput, BlockExecutionError> {
        let log = crate::failure_receipt::build_outbe_failure_log(
            input.log_address,
            input.code,
            input.reason,
        );
        self.inner.receipts.push(Receipt {
            tx_type: input.tx_type,
            success: false,
            cumulative_gas_used: self.inner.cumulative_tx_gas_used,
            logs: vec![log],
        });
        self.system_tx_execution_gas = self
            .system_tx_execution_gas
            .saturating_add(input.internal_gas_used);
        self.system_tx_gross_gas = self
            .system_tx_gross_gas
            .saturating_add(input.internal_gross_gas_used);
        Ok(GasOutput::new(0))
    }
}
