//! Gas admission and scope state for the context-backed storage provider.

use super::{
    CtxStorageProvider, Debug, PrecompileError, PrecompileStorageProvider, Result, SubCallError,
};
use outbe_primitives::storage::gas::{TriggerBudget, TriggerScopeError};
use revm::Database;

impl<DB: Database + Debug> CtxStorageProvider<'_, DB> {
    pub(super) fn charge(&mut self, cost: u64) -> Result<()> {
        if self.trigger_budget.is_some() {
            self.trigger_budget_status()?;
            if cost > self.gas.remaining() {
                self.subcall_out_of_gas = true;
                self.gas.inner_mut().spend_all();
                return Err(PrecompileError::OutOfGas);
            }
            if let Some(budget) = &mut self.trigger_budget {
                budget.check(cost)?;
            }
        }
        if let Some(child) = self.gas_child_remaining {
            if cost > child {
                let _ = self.gas.record_regular_cost(child);
                if let Some(budget) = &mut self.trigger_budget {
                    budget.charge(child)?;
                }
                self.gas_child_remaining = Some(0);
                return Err(PrecompileError::OutOfGas);
            }
            self.gas_child_remaining = Some(child - cost);
        }
        if self.gas.record_regular_cost(cost) {
            if let Some(budget) = &mut self.trigger_budget {
                budget.charge(cost)?;
            }
            Ok(())
        } else {
            Err(PrecompileError::OutOfGas)
        }
    }

    pub(super) fn charge_subcall(&mut self, cost: u64) -> std::result::Result<(), SubCallError> {
        if self.trigger_budget.is_some() {
            return self.charge(cost).map_err(subcall_error);
        }
        if self.gas.record_regular_cost(cost) {
            Ok(())
        } else {
            self.subcall_out_of_gas = true;
            self.gas.inner_mut().spend_all();
            Err(SubCallError::ParentOutOfGas)
        }
    }

    pub(super) fn start_trigger_budget(&mut self, budget: u64) -> Result<()> {
        if self.trigger_budget.is_some() {
            return Err(TriggerScopeError::Nested.into());
        }
        if self.subcall_out_of_gas {
            return Err(PrecompileError::OutOfGas);
        }
        self.trigger_budget = Some(TriggerBudget::new(budget));
        Ok(())
    }

    pub(super) fn active_trigger_status(&self) -> Result<()> {
        if self.subcall_out_of_gas {
            return Err(PrecompileError::OutOfGas);
        }
        self.trigger_budget
            .as_ref()
            .ok_or(TriggerScopeError::Inactive)?
            .status()
    }

    pub(super) fn finish_trigger_budget(&mut self) -> Result<()> {
        self.trigger_budget
            .take()
            .ok_or(TriggerScopeError::Inactive)?;
        Ok(())
    }

    pub(super) fn observe_trigger_failure(&mut self, error: &PrecompileError) {
        if let Some(budget) = &mut self.trigger_budget {
            budget.observe_error(error);
        }
    }
}

fn subcall_error(error: PrecompileError) -> SubCallError {
    match error {
        PrecompileError::TriggerBudgetExceeded {
            consumed_gas,
            budget_gas,
        } => SubCallError::TriggerBudgetExceeded {
            consumed_gas,
            budget_gas,
        },
        PrecompileError::OutOfGas => SubCallError::ParentOutOfGas,
        other => SubCallError::Fatal(other.to_string()),
    }
}
