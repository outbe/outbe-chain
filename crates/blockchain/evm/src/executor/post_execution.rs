//! Standard Ethereum post-execution, run before the OCOMP terminal boundary.

use super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// Runs the standard Ethereum post-execution phase before the OCOMP
    /// terminal boundary.
    ///
    /// This intentionally mirrors [`EthBlockExecutor::finish`]'s semantic
    /// writes. The active OCOMP lifecycle requires a stricter order than the
    /// upstream executor exposes:
    ///
    /// `Ethereum post-execution -> CE preview -> OSR2 -> final CE seal`.
    ///
    /// The resulting EIP-7685 requests are retained for [`BlockExecutor::finish`],
    /// which assembles the result without invoking the upstream phase again.
    pub(in crate::executor) fn apply_outbe_ethereum_post_execution(
        &mut self,
    ) -> Result<(), BlockExecutionError> {
        if self.ethereum_post_execution_requests.is_some() {
            return Err(BlockExecutionError::msg(
                "standard Ethereum post-execution changes already applied",
            ));
        }

        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

        let requests = if self
            .inner
            .spec
            .is_prague_active_at_timestamp(self.inner.evm.block().timestamp().saturating_to())
        {
            let deposit_requests =
                eip6110::parse_deposits_from_receipts(self.inner.spec, &self.inner.receipts)?;
            let mut requests = Requests::default();
            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }
            self.inner
                .system_caller
                .append_post_execution_changes(&mut self.inner.evm, &mut requests)?;
            requests
        } else {
            Requests::default()
        };

        let mut balance_increments = post_block_balance_increments(
            self.inner.spec,
            self.inner.evm.block(),
            self.inner.ctx.ommers,
            None,
        );

        if self
            .inner
            .spec
            .ethereum_fork_activation(EthereumHardfork::Dao)
            .transitions_at_block(self.inner.evm.block().number().saturating_to())
        {
            let drained_balance: u128 = self
                .inner
                .evm
                .db_mut()
                .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS)
                .map_err(|_| BlockValidationError::IncrementBalanceFailed)?
                .into_iter()
                .sum();
            *balance_increments
                .entry(dao_fork::DAO_HARDFORK_BENEFICIARY)
                .or_default() += drained_balance;
        }

        self.inner
            .evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        self.ethereum_post_execution_requests = Some(requests);
        Ok(())
    }
}
