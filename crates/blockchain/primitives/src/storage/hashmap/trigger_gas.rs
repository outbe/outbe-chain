//! Production-shaped gas accounting for the in-memory fixture provider.

use super::HashMapStorageProvider;
use crate::{
    error::{PrecompileError, Result},
    storage::{
        gas::{TriggerBudget, TriggerScopeError},
        PrecompileStorageProvider,
    },
};

impl HashMapStorageProvider {
    pub(super) fn charge_gas(&mut self, gas: u64) -> Result<()> {
        self.check_trigger_charge(gas)?;
        self.charge_legacy_ceiling(gas)?;
        self.charge_parent_gas(gas)
    }

    fn check_trigger_charge(&mut self, gas: u64) -> Result<()> {
        if self.trigger_budget.is_none() {
            return Ok(());
        }
        self.trigger_budget_status()?;
        if self
            .metered_gas_remaining()
            .is_some_and(|remaining| gas > remaining)
        {
            self.parent_out_of_gas = true;
            if let Some(limit) = self.gas_limit {
                self.gas_used = limit;
            }
            return Err(PrecompileError::OutOfGas);
        }
        if let Some(budget) = &mut self.trigger_budget {
            budget.check(gas)?;
        }
        Ok(())
    }

    fn charge_legacy_ceiling(&mut self, gas: u64) -> Result<()> {
        if let Some(child) = self.gas_child_remaining {
            if gas > child {
                if let Some(limit) = self.gas_limit {
                    self.gas_used = self.gas_used.saturating_add(child).min(limit);
                }
                self.gas_child_remaining = Some(0);
                if let Some(budget) = &mut self.trigger_budget {
                    budget.charge(child)?;
                }
                return Err(PrecompileError::OutOfGas);
            }
            self.gas_child_remaining = Some(child - gas);
        }
        Ok(())
    }

    fn charge_parent_gas(&mut self, gas: u64) -> Result<()> {
        let Some(limit) = self.gas_limit else {
            return Ok(());
        };
        let next = self
            .gas_used
            .checked_add(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        if next > limit {
            return Err(PrecompileError::OutOfGas);
        }
        self.gas_used = next;
        if let Some(budget) = &mut self.trigger_budget {
            budget.charge(gas)?;
        }
        Ok(())
    }

    pub(super) fn start_trigger_budget(&mut self, budget: u64) -> Result<()> {
        if self.trigger_budget.is_some() {
            return Err(TriggerScopeError::Nested.into());
        }
        if self.gas_limit.is_none() || !self.meter_storage_gas {
            return Err(TriggerScopeError::Unsupported.into());
        }
        if self.parent_out_of_gas {
            return Err(PrecompileError::OutOfGas);
        }
        self.trigger_budget = Some(TriggerBudget::new(budget));
        Ok(())
    }

    pub(super) fn active_trigger_status(&self) -> Result<()> {
        if self.parent_out_of_gas {
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

    pub(super) fn start_gas_ceiling(&mut self, ceiling: u64) -> Result<()> {
        if let Some(limit) = self.gas_limit {
            if limit.saturating_sub(self.gas_used) < ceiling {
                return Err(PrecompileError::OutOfGas);
            }
        }
        self.gas_child_remaining = Some(ceiling);
        Ok(())
    }
}
