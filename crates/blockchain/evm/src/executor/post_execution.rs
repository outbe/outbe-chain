//! Standard Ethereum post-execution with native Outbe withdrawal validation.

use super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// Applies Ethereum post-execution writes without Gwei conversion of native Outbe withdrawals.
    /// Retains EIP-7685 requests for final result assembly.
    pub fn apply_outbe_ethereum_post_execution(&mut self) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        if self.ethereum_post_execution_requests.is_some() {
            return Err(ExecutorValidationError::PostExecutionAlreadyApplied.into());
        }

        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(ExecutorValidationError::invalid_withdrawals)?;

        let requests = if self.inner.spec.is_prague_active_at_timestamp(timestamp) {
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

        self.settle_block_burned_fees()?;

        self.ethereum_post_execution_requests = Some(requests);
        Ok(())
    }

    /// Settles accumulated block burned fees (`current_block_burned_fees`) through the runtime
    /// host, then flushes storage to the underlying `State` so any attached `OnStateHook`
    /// receives the transition before parallel root freeze.
    pub fn settle_block_burned_fees(&mut self) -> Result<(), BlockExecutionError> {
        use crate::errors::BurnedFeeSettlementError;
        use outbe_primitives::error::PrecompileError;
        use outbe_primitives::storage::{direct::DirectStorageProvider, StorageHandle};

        if self.burned_fees_settled {
            return Ok(());
        }

        let burned_fees = self.current_block_burned_fees;
        if burned_fees.is_zero() {
            self.burned_fees_settled = true;
            return Ok(());
        }

        let block_ctx = self.fee_route_context()?;
        let host = self.runtime_host.clone();
        let mut provider = DirectStorageProvider::new(self.inner.evm.db_mut(), block_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            host.settle_block_burned_fees(storage, burned_fees)
        })
        .map_err(|err| {
            if err.is_node_local()
                || matches!(
                    err,
                    PrecompileError::Fatal(_) | PrecompileError::FatalWithCause(_)
                )
            {
                BlockExecutionError::from(BurnedFeeSettlementError::Internal(err))
            } else {
                BlockExecutionError::from(BurnedFeeSettlementError::Validation(err))
            }
        })?;
        provider
            .flush()
            .map_err(|err| BlockExecutionError::from(BurnedFeeSettlementError::Flush(err)))?;
        // End-of-block event publication is deferred to the separate Events review (OIP-00309 §3.1 / §5.2).
        let _events = provider.take_events();
        self.burned_fees_settled = true;
        Ok(())
    }
}
