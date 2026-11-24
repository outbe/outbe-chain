//! CALL admission, frame execution, and canonical allowance settlement.

use super::{
    CtxStorageProvider, PrecompileError, SubCallError, SubCallInput, SubCallOutput, SubCallStatus,
};
use crate::{factory::OutbeEvmTypes, sub_call};
use alloy_primitives::Address;
use evm2::{
    bytecode::Bytecode,
    interpreter::{op, Host as _, InstrStop, MessageResult},
    version::GasId,
    EvmFeatures, HostError,
};

impl CtxStorageProvider<'_, '_> {
    pub(super) fn execute_subcall(
        &mut self,
        mut input: SubCallInput,
    ) -> Result<SubCallOutput, SubCallError> {
        self.check_subcall_admission()?;
        let is_static = self.is_static || input.is_static;
        if is_static && !input.value.is_zero() {
            return Ok(halted_entry(SubCallError::StateChangeDuringStaticCall));
        }
        let loaded = self.charge_subcall_access(&input, is_static)?;
        let locally_limited = self.reserve_subcall_allowance(&mut input)?;
        let allowance = input.gas_limit;
        let outcome = self.run_subcall_frame(input, loaded);
        match outcome {
            Ok(outcome) => self.settle_subcall(outcome, locally_limited),
            Err(error @ SubCallError::DepthLimitExceeded) => {
                self.gas.erase_cost(allowance);
                self.return_subcall_allowance(allowance)?;
                Ok(halted_entry(error))
            }
            Err(error) => Err(error),
        }
    }

    fn check_subcall_admission(&mut self) -> Result<(), SubCallError> {
        self.abort_bridge.check_subcall()?;
        if self.trigger_budget.is_some() {
            self.charge_subcall(0)?;
        }
        // The boundary has a single gas pool; do not discard EIP-8037 charges.
        if self.evm.feature(EvmFeatures::EIP8037) {
            return Err(SubCallError::Fatal(
                "Outbe subcalls do not support EIP-8037".into(),
            ));
        }
        Ok(())
    }

    fn charge_subcall_access(
        &mut self,
        input: &SubCallInput,
        is_static: bool,
    ) -> Result<(Address, Bytecode), SubCallError> {
        // Match pinned CALL access pricing before EIP-150, without its automatic value stipend.
        let opcode = if is_static { op::STATICCALL } else { op::CALL };
        let static_gas = u64::from(
            self.evm
                .execution_config()
                .opcode_config()
                .static_gas(opcode),
        );
        self.charge_subcall(static_gas)?;
        let gas_params = self.evm.version().gas_params;
        let eip7702 = self.evm.feature(EvmFeatures::EIP7702);
        let eip161 = self.evm.feature(EvmFeatures::EIP161);
        let transfers_value = !input.value.is_zero();
        if transfers_value {
            self.charge_subcall(gas_params.get(GasId::TransferValueCost))?;
        }
        let additional_cold_cost = gas_params.cold_account_additional_cost();
        let remaining_gas = self.gas.remaining();
        let skip_cold_load = remaining_gas < additional_cold_cost;
        let account = self.load_subcall_account(input.target, skip_cold_load)?;
        let mut cost = if account.is_cold {
            additional_cold_cost
        } else {
            0
        };
        let mut code = account.code;
        let mut code_address = input.target;
        if eip7702 {
            if let Some(delegated_address) = code.eip7702_address() {
                cost += gas_params.get(GasId::WarmStorageReadCost);
                if cost > remaining_gas {
                    let _ = self.charge_subcall(u64::MAX);
                    return Err(SubCallError::ParentOutOfGas);
                }
                let skip_delegate_cold = remaining_gas < cost.saturating_add(additional_cold_cost);
                let delegated = self.load_subcall_account(delegated_address, skip_delegate_cold)?;
                if delegated.is_cold {
                    cost += additional_cold_cost;
                }
                code = delegated.code;
                code_address = delegated_address;
            }
        }
        if !is_static && account.is_empty_for_new_account_gas && (!eip161 || transfers_value) {
            cost += gas_params.get(GasId::NewAccountCost);
        }
        self.charge_subcall(cost)?;
        Ok((code_address, code))
    }

    fn load_subcall_account(
        &mut self,
        address: Address,
        skip_cold_load: bool,
    ) -> Result<evm2::evm::AccountLoad, SubCallError> {
        match self.evm.load_account(&address, true, skip_cold_load) {
            Ok(loaded) => Ok(loaded),
            Err(HostError::Halt(InstrStop::OutOfGas)) => {
                let _ = self.charge_subcall(u64::MAX);
                Err(SubCallError::ParentOutOfGas)
            }
            Err(HostError::Execution(err)) => {
                Err(crate::errors::SubCallDriverError::database_error(err))
            }
            Err(HostError::Halt(other)) => Err(SubCallError::EvmHalt(other)),
        }
    }

    fn reserve_subcall_allowance(
        &mut self,
        input: &mut SubCallInput,
    ) -> Result<bool, SubCallError> {
        let remaining = self.gas.remaining();
        let cap = if self.evm.feature(EvmFeatures::EIP150) {
            self.evm
                .version()
                .gas_params
                .call_stipend_reduction(remaining)
        } else {
            remaining
        };
        input.gas_limit = input.gas_limit.min(cap);
        if self.trigger_budget.is_some() {
            if let Some(legacy_remaining) = self.gas_child_remaining {
                input.gas_limit = input.gas_limit.min(legacy_remaining);
            }
        }
        let locally_limited = self
            .trigger_budget
            .as_ref()
            .is_some_and(|budget| budget.remaining < input.gas_limit);
        if let Some(budget) = &self.trigger_budget {
            input.gas_limit = input.gas_limit.min(budget.remaining);
        }
        self.charge_subcall(input.gas_limit)?;
        Ok(locally_limited)
    }

    fn run_subcall_frame(
        &mut self,
        input: SubCallInput,
        loaded: (Address, Bytecode),
    ) -> Result<MessageResult<OutbeEvmTypes>, SubCallError> {
        sub_call::run_frame(
            self.evm,
            sub_call::SubCallContext {
                self_address: self.self_address,
                outer_is_static: self.is_static,
                runtime: crate::precompiles::OutbePrecompileRuntime::new()
                    .with_abort_bridge(self.abort_bridge.clone()),
                host: std::sync::Arc::clone(&self.host),
            },
            input,
            Some(loaded),
        )
    }

    fn settle_subcall(
        &mut self,
        outcome: MessageResult<OutbeEvmTypes>,
        locally_limited: bool,
    ) -> Result<SubCallOutput, SubCallError> {
        // Public OOG includes fixed memory/operand failures that more allowance cannot fix.
        let allowance_exhausted = matches!(
            outcome.stop,
            InstrStop::OutOfGas
                | InstrStop::MemoryOOG
                | InstrStop::PrecompileOOG
                | InstrStop::ReentrancySentryOOG
        );
        let reserved_remaining = self.gas.remaining();
        let output = sub_call::settle_outcome(outcome, self.gas.inner_mut())?;
        // Canonical settlement has already returned parent gas; mirror only its actual return.
        self.return_subcall_allowance(self.gas.remaining() - reserved_remaining)?;
        if locally_limited && allowance_exhausted {
            self.exhaust_subcall_budget()?;
        }
        Ok(output)
    }

    fn return_subcall_allowance(&mut self, unused: u64) -> Result<(), SubCallError> {
        if let Some(budget) = &mut self.trigger_budget {
            budget
                .return_unused(unused)
                .map_err(|error| SubCallError::Fatal(error.to_string()))?;
            if let Some(child) = &mut self.gas_child_remaining {
                *child = child
                    .checked_add(unused)
                    .ok_or_else(|| SubCallError::Fatal("legacy gas return overflow".into()))?;
            }
        }
        Ok(())
    }

    fn exhaust_subcall_budget(&mut self) -> Result<(), SubCallError> {
        if let Some(budget) = &mut self.trigger_budget {
            budget.exhaust();
            if let Err(PrecompileError::TriggerBudgetExceeded {
                consumed_gas,
                budget_gas,
            }) = budget.status()
            {
                return Err(SubCallError::TriggerBudgetExceeded {
                    consumed_gas,
                    budget_gas,
                });
            }
        }
        Ok(())
    }
}

fn halted_entry(error: SubCallError) -> SubCallOutput {
    SubCallOutput {
        status: SubCallStatus::Halt(error),
        returndata: Default::default(),
        gas_used: 0,
        gas_refunded: 0,
    }
}
