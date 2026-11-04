//! Pre-end admission and the empty terminal execution phase.

use super::*;
use crate::errors::ExecutorValidationError;
use outbe_primitives::system_tx::{BlockGasBudget, SystemTxPhase};

impl<E: Evm> OutbeBlockExecutor<'_, E> {
    fn block_gas_budget(&self) -> Result<BlockGasBudget, BlockExecutionError> {
        BlockGasBudget::new(self.inner.evm.block().gas_limit())
            .map_err(|error| ExecutorValidationError::from(error).into())
    }

    fn accounted_block_gas(&self) -> u64 {
        if self.inner.evm.cfg_env().enable_amsterdam_eip8037 {
            self.inner.max_block_gas_used()
        } else {
            self.inner.cumulative_tx_gas_used
        }
    }

    pub(in crate::executor) fn ensure_before_end_block(&self) -> Result<(), BlockExecutionError> {
        if matches!(self.system_tx_phase_cursor, SystemTxPhase::EndBlock { .. }) {
            return Err(ExecutorValidationError::TransactionAfterEndBlock.into());
        }
        Ok(())
    }

    /// Builder stage after user transactions and before execution finalization.
    /// Repeated calls retain the captured capacity.
    pub fn complete_end_block_phase(&mut self) -> Result<(), BlockExecutionError> {
        if matches!(self.system_tx_phase_cursor, SystemTxPhase::EndBlock { .. }) {
            return Ok(());
        }
        let capacity = self
            .block_gas_budget()?
            .end_capacity(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        // No end selectors exist yet. This phase intentionally creates no state,
        // receipt, fee, log, or witness entry.
        self.system_tx_phase_cursor = SystemTxPhase::EndBlock {
            gas_capacity: capacity,
        };
        Ok(())
    }

    pub(in crate::executor) fn check_visible_pre_end_gas(
        &self,
        additional: u64,
    ) -> Result<(), BlockExecutionError> {
        self.ensure_before_end_block()?;
        let available = self
            .block_gas_budget()?
            .remaining_pre_end_gas(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        check_capacity(additional, additional, available)
    }

    pub(in crate::executor) fn check_user_pre_end_gas(
        &self,
        gas_limit: u64,
    ) -> Result<(), BlockExecutionError>
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        use revm::context_interface::Cfg;
        self.ensure_before_end_block()?;
        let budget = self.block_gas_budget()?;
        let amsterdam = self.inner.evm.cfg_env().enable_amsterdam_eip8037;
        let regular_used = if amsterdam {
            self.inner.block_regular_gas_used
        } else {
            self.inner.cumulative_tx_gas_used
        };
        let available = budget
            .remaining_pre_end_gas(regular_used)
            .map_err(ExecutorValidationError::from)?;
        let regular_limit = gas_limit.min(self.inner.evm.cfg_env().tx_gas_limit_cap());
        check_capacity(gas_limit, regular_limit, available)?;
        if amsterdam {
            let available = budget
                .remaining_pre_end_gas(self.inner.block_state_gas_used)
                .map_err(ExecutorValidationError::from)?;
            check_capacity(gas_limit, gas_limit, available)?;
        }
        Ok(())
    }

    pub(in crate::executor) fn check_candidate_pre_end_gas(
        &self,
        is_system: bool,
        gas_limit: u64,
    ) -> Result<(), BlockExecutionError>
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        if is_system {
            self.ensure_before_end_block()
        } else {
            self.check_user_pre_end_gas(gas_limit)
        }
    }
}

fn check_capacity(declared: u64, required: u64, available: u64) -> Result<(), BlockExecutionError> {
    if required > available {
        return Err(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: declared,
                block_available_gas: available,
            }
            .into(),
        );
    }
    Ok(())
}
