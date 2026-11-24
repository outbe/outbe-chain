//! Standard Ethereum post-execution with native Outbe withdrawal validation.

use super::*;

impl OutbeBlockExecutor<'_> {
    /// Applies Ethereum post-execution writes without Gwei conversion of native Outbe withdrawals.
    /// Retains EIP-7685 requests for final result assembly.
    pub fn apply_outbe_ethereum_post_execution(&mut self) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let _timestamp = outbe_primitives::time::timestamp_secs(self.block_timestamp_u256())
            .map_err(ExecutorValidationError::from)?;
        if self.ethereum_post_execution_requests.is_some() {
            return Err(ExecutorValidationError::PostExecutionAlreadyApplied.into());
        }

        validate_outbe_withdrawals(self.withdrawals.as_deref())
            .map_err(ExecutorValidationError::invalid_withdrawals)?;

        // `self.inner` was constructed with `withdrawals: None`, so Ethereum's post-execution
        // applies EIP-6110 deposit parsing, block/ommer rewards, and EIP-7002/7251 request system
        // calls (streaming every StateUpdate to any installed state hook) without converting
        // Outbe's native withdrawals as Gwei.
        let post_output = self.inner.inner.apply_post_execution_changes(0)?;

        self.settle_block_burned_fees()?;

        self.ethereum_post_execution_requests = Some(post_output.requests);
        Ok(())
    }

    /// Settles accumulated block burned fees (`current_block_burned_fees`) through the runtime
    /// host, then commits the resulting [`BlockStateAccumulator`] into `self.inner.block_state`
    /// and any attached `StateUpdate` hook before parallel root freeze.
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
        let changes = {
            let db = self.inner.evm_mut().overlay_db_mut();
            let mut provider = DirectStorageProvider::new(db, block_ctx);
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
            provider.take_committed_changes()
        };
        self.commit_block_accumulator(&changes)?;
        self.burned_fees_settled = true;
        Ok(())
    }
}
