//! Paid user execution and checked validator-fee accumulation.

use super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub(super) fn execute_paid_transaction<R, F>(
        &mut self,
        tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let tx = recovered.tx();
        let base_fee_per_gas = self.inner.evm.block().basefee() as u128;
        let max_fee_per_gas = tx.max_fee_per_gas();
        let max_priority_fee_per_gas = tx.max_priority_fee_per_gas();

        let result = self.inner.execute_transaction_with_commit_condition(
            WithTxEnv {
                tx_env,
                tx: Arc::new(recovered),
            },
            f,
        )?;

        if let Some(gas_used) = result {
            let validator_fee = validator_fee_for_gas(
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_used.tx_gas_used(),
                base_fee_per_gas,
            );
            self.current_block_validator_fees = self
                .current_block_validator_fees
                .checked_add(validator_fee)
                .ok_or_else(|| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        "validator fee accumulator overflow".into(),
                    ))
                })?;
        }

        Ok(result)
    }
}
