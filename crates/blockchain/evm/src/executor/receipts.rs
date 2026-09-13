use super::*;

pub(in crate::executor) struct SystemFailureReceiptInput {
    pub(super) tx_type: alloy_consensus::TxType,
    pub(super) log_address: Address,
    pub(super) code: u16,
    pub(super) reason: String,
    pub(super) visible_base_gas: u64,
    pub(super) compressed_entities_gas: u64,
    pub(super) signed_gas_limit: u64,
    pub(super) internal_gas_used: u64,
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
    /// `(log_address, code, reason)`; identical inputs across proposer
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

    /// Pushes a `status=1` synthetic receipt for the mandatory `HookEvents` system
    /// tx, carrying whitelisted pre-exec hook logs without re-running lifecycle hooks.
    pub(crate) fn push_hook_events_receipt(
        &mut self,
        tx_type: alloy_consensus::TxType,
        logs: Vec<Log>,
        visible_gas_used: u64,
    ) -> Result<GasOutput, BlockExecutionError> {
        let user_cumulative_tx_gas = self
            .inner
            .cumulative_tx_gas_used
            .checked_add(visible_gas_used)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "HookEvents visible gas overflow".into(),
                ))
            })?;
        self.inner.receipts.push(Receipt {
            tx_type,
            success: true,
            cumulative_gas_used: user_cumulative_tx_gas,
            logs,
        });
        self.inner.cumulative_tx_gas_used = user_cumulative_tx_gas;
        self.inner.block_regular_gas_used = self
            .inner
            .block_regular_gas_used
            .saturating_add(visible_gas_used);
        self.inner.block_state_gas_used = self
            .inner
            .block_state_gas_used
            .saturating_add(visible_gas_used);
        Ok(GasOutput::new(visible_gas_used))
    }
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    pub(in crate::executor) fn visible_system_gas_with_compressed_entities(
        &self,
        visible_base_gas: u64,
        compressed_entities_gas: u64,
        signed_gas_limit: u64,
    ) -> Result<u64, BlockExecutionError> {
        let visible_gas = visible_base_gas
            .checked_add(compressed_entities_gas)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "system tx visible gas overflow after compressed-entity charge".into(),
                ))
            })?;
        if visible_gas > signed_gas_limit {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "system tx receipt gas exceeds signed gas limit: visible={visible_gas}, \
                         signed_limit={signed_gas_limit}, visible_base={visible_base_gas}, \
                         compressed_entities={compressed_entities_gas}"
                    )
                    .into(),
                ),
            ));
        }
        let cumulative = self
            .inner
            .cumulative_tx_gas_used
            .checked_add(visible_gas)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "system tx cumulative visible gas overflow".into(),
                ))
            })?;
        let block_gas_limit = self.inner.evm.block().gas_limit();
        if cumulative > block_gas_limit {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "system tx visible gas exceeds block gas limit: cumulative={cumulative}, \
                         block_limit={block_gas_limit}, visible_base={visible_base_gas}, \
                         compressed_entities={compressed_entities_gas}"
                    )
                    .into(),
                ),
            ));
        }
        Ok(visible_gas)
    }

    /// Pushes a `status=0` system synthetic receipt and publishes only the
    /// signed envelope plus explicit CE gas; unrelated internal-lane work
    /// remains hidden.
    pub(in crate::executor) fn push_system_failure_receipt(
        &mut self,
        input: SystemFailureReceiptInput,
    ) -> Result<GasOutput, BlockExecutionError> {
        let visible_gas_used = self.visible_system_gas_with_compressed_entities(
            input.visible_base_gas,
            input.compressed_entities_gas,
            input.signed_gas_limit,
        )?;
        let log = crate::failure_receipt::build_outbe_failure_log(
            input.log_address,
            input.code,
            input.reason,
        );
        let system_cumulative_gas_used =
            self.checked_system_tx_execution_gas(input.internal_gas_used)?;
        let user_cumulative_gas_used = self
            .inner
            .cumulative_tx_gas_used
            .checked_add(visible_gas_used)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "system tx failure visible gas overflow".into(),
                ))
            })?;
        self.inner.receipts.push(Receipt {
            tx_type: input.tx_type,
            success: false,
            cumulative_gas_used: user_cumulative_gas_used,
            logs: vec![log],
        });
        self.system_tx_execution_gas = system_cumulative_gas_used;
        self.inner.cumulative_tx_gas_used = user_cumulative_gas_used;
        self.inner.block_regular_gas_used = self
            .inner
            .block_regular_gas_used
            .saturating_add(visible_gas_used);
        self.inner.block_state_gas_used = self
            .inner
            .block_state_gas_used
            .saturating_add(visible_gas_used);
        Ok(GasOutput::new(visible_gas_used))
    }
}
