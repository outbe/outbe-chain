//! User transaction fee route: protocol waiver, delegation bootstrap, sponsorship, paid.

use super::*;

mod paid;
mod sponsorship;
mod waiver;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub fn route_user_transaction<R, F>(
        &mut self,
        tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let ctx = self.fee_route_context()?;
        let tx = recovered.tx();
        let signer = *recovered.signer();
        self.validate_user_tx_gas_bounds(tx.gas_limit())?;

        let zero_fee_tx = zero_fee_transaction(tx, signer);
        let candidate = self.runtime_host.user_fee_policy().classify(&zero_fee_tx);
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => return self.include_zero_fee_soft_failure(tx, error, None),
        };
        if let Some(candidate) = candidate {
            return self.execute_protocol_fee_waiver(candidate, tx_env, recovered, f);
        }

        // Preserve the signer read before both bootstrap and paid routing.
        let account = self.read_fee_route_account(signer, ctx.clone())?;
        if waiver::bootstrap_fee_waiver_authorized(
            self.runtime_host.user_fee_policy(),
            tx,
            signer,
            ctx.chain_id,
            &account,
        ) {
            let gas_limit = tx.gas_limit();
            return if self.sponsored_gas_fits(gas_limit) {
                let result = self.execute_without_user_fee(tx_env, recovered, f);
                if matches!(result, Ok(Some(_))) {
                    self.charge_sponsored_gas(gas_limit);
                }
                result
            } else {
                Err(crate::sponsored_block_budget_deferral(*tx.tx_hash()))
            };
        }

        let delegated_to = self.fee_route_delegation(account)?;
        // Delegation is additive: an envelope that opts into paid execution
        // still takes the normal fee path, even when sponsorship is exhausted.
        let wants_sponsorship = delegated_to == Some(outbe_primitives::addresses::ZEROFEE_ADDRESS)
            && self
                .runtime_host
                .user_fee_policy()
                .classify_sponsorship(&zero_fee_tx)
                .is_ok();
        if wants_sponsorship {
            return self.execute_sponsored_transaction(ctx, tx_env, recovered, f);
        }

        self.execute_paid_transaction(tx_env, recovered, f)
    }

    fn validate_user_tx_gas_bounds(&self, gas_limit: u64) -> Result<(), BlockExecutionError> {
        if gas_limit < Self::SOFT_FAILURE_GAS {
            return Err(
                crate::errors::ExecutorValidationError::TransactionGasLimitBelowFloor {
                    gas_limit,
                    floor: Self::SOFT_FAILURE_GAS,
                }
                .into(),
            );
        }
        let block_gas_limit = self.inner.evm.block().gas_limit();
        let available = block_gas_limit.saturating_sub(self.accounted_block_gas());
        if gas_limit > available {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: gas_limit,
                    block_available_gas: available,
                }
                .into(),
            );
        }
        Ok(())
    }

    /// The executor includes policy rejections with fixed gas and a failure log,
    /// up to the shared per-block cap. Account for the cap before appending a receipt.
    fn include_zero_fee_soft_failure(
        &mut self,
        tx: &TransactionSigned,
        error: outbe_primitives::zero_fee_types::ZeroFeePolicyError,
        candidate: Option<outbe_primitives::zero_fee_types::ZeroFeeCandidate>,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let block_gas_limit = self.inner.evm.block().gas_limit();
        let used = self.accounted_block_gas();
        let available = block_gas_limit.saturating_sub(used);
        if Self::SOFT_FAILURE_GAS > available {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: Self::SOFT_FAILURE_GAS,
                    block_available_gas: available,
                }
                .into(),
            );
        }
        self.record_zero_fee_soft_failure(*tx.tx_hash(), candidate)?;
        self.push_failure_receipt(
            tx.tx_type(),
            outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS,
            error.code(),
            error.to_string(),
        );
        Ok(Some(GasOutput::new(Self::SOFT_FAILURE_GAS)))
    }
}

impl<E: Evm> OutbeBlockExecutor<'_, E> {
    pub fn fee_route_context(&self) -> Result<BlockContext, BlockExecutionError> {
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        Ok(self.block_context(
            self.inner.evm.block().number().saturating_to::<u64>(),
            timestamp,
            self.inner.evm.block().beneficiary(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::zero_fee_types::{ZeroFeeCandidate, ZeroFeeHookTag, ZeroFeePolicyError};

    #[test]
    fn zero_fee_tail_gas_rejection_preserves_caps_and_receipts() {
        super::super::zero_fee::tests::with_executor(|mut executor| {
            let tx: TransactionSigned = alloy_consensus::TxLegacy::default()
                .into_signed(alloy_primitives::Signature::test_signature())
                .into();
            let candidate = ZeroFeeCandidate::new(
                ZeroFeeHookTag::new(3, "SlashEquivocation", Some(1))
                    .with_max_soft_failures_per_block(4),
                Address::ZERO,
            );
            let limit = executor.inner.evm.block().gas_limit();
            let used = limit - 20_999;
            executor.inner.cumulative_tx_gas_used = used;
            assert!(matches!(
                executor.include_zero_fee_soft_failure(
                    &tx,
                    ZeroFeePolicyError::NonZeroValue,
                    Some(candidate),
                ),
                Err(BlockExecutionError::Validation(
                    BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                        transaction_gas_limit: 21_000,
                        block_available_gas: 20_999,
                    }
                ))
            ));
            assert_eq!(executor.zero_fee_soft_failures, 0);
            assert!(executor.zero_fee_hook_soft_failures.is_empty());
            assert!(executor.inner.receipts.is_empty());
            assert_eq!(executor.gas_accounting(), (used, 0, 0));
            executor.inner.cumulative_tx_gas_used = limit - 21_000;
            assert!(executor
                .include_zero_fee_soft_failure(
                    &tx,
                    ZeroFeePolicyError::NonZeroValue,
                    Some(candidate),
                )
                .unwrap()
                .is_some());
            assert_eq!(executor.zero_fee_soft_failures, 1);
            assert_eq!(executor.zero_fee_hook_soft_failures.get(&3), Some(&1));
            assert_eq!(executor.gas_accounting(), (limit, 21_000, 21_000));
            let receipt = &executor.inner.receipts[0];
            assert!(!receipt.success);
            assert_eq!(receipt.cumulative_gas_used, limit);
            assert_eq!(receipt.logs.len(), 1);
        });
    }
}
