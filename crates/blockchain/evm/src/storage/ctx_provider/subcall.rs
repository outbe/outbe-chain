//! CALL admission, frame execution, and canonical allowance settlement.

use super::{
    CtxStorageProvider, Debug, PrecompileError, SubCallError, SubCallInput, SubCallOutput,
    SubCallStatus,
};
use crate::sub_call;
use alloy_primitives::B256;
use revm::{
    context_interface::{host::LoadError, Cfg as _, ContextTr},
    interpreter::{
        instructions::{contract::load_account_delegated, gas_table_spec},
        CallOutcome, InstructionResult,
    },
    primitives::hardfork::SpecId,
    state::Bytecode,
    Database,
};

impl<DB: Database + Debug> CtxStorageProvider<'_, DB> {
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
        if self.ctx.cfg().is_amsterdam_eip8037_enabled() {
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
    ) -> Result<(B256, Bytecode), SubCallError> {
        // Match pinned CALL access pricing before EIP-150, without its automatic value stipend.
        let opcode = if is_static { 0xfa } else { 0xf1 };
        self.charge_subcall(u64::from(gas_table_spec(self.spec)[opcode]))?;
        let transfers_value = !input.value.is_zero();
        if transfers_value {
            self.charge_subcall(self.ctx.cfg().gas_params().transfer_value_cost())?;
        }
        let (cost, state_cost, code, hash) = match load_account_delegated(
            self.ctx,
            self.spec,
            self.gas.remaining(),
            input.target,
            transfers_value,
            !is_static,
        ) {
            Ok(loaded) => loaded,
            Err(LoadError::ColdLoadSkipped) => {
                self.charge_subcall(u64::MAX)?;
                return Err(SubCallError::ParentOutOfGas);
            }
            Err(LoadError::DBError) => {
                return Err(crate::errors::SubCallDriverError::database_error(
                    self.ctx.error(),
                ));
            }
        };
        if state_cost != 0 {
            return Err(crate::errors::SubCallDriverError::UnsupportedStateGasCharge.into());
        }
        self.charge_subcall(cost)?;
        Ok((hash, code))
    }

    fn reserve_subcall_allowance(
        &mut self,
        input: &mut SubCallInput,
    ) -> Result<bool, SubCallError> {
        let remaining = self.gas.remaining();
        let cap = if self.spec.is_enabled_in(SpecId::TANGERINE) {
            self.ctx
                .cfg()
                .gas_params()
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
        loaded: (B256, Bytecode),
    ) -> Result<CallOutcome, SubCallError> {
        sub_call::run_frame(
            self.ctx,
            sub_call::SubCallContext {
                self_address: self.self_address,
                outer_is_static: self.is_static,
                execution: crate::precompiles::OutbePrecompileExecutionContext::new(
                    self.spec,
                    self.genesis_hash,
                )
                .with_genesis_timestamp(self.genesis_timestamp),
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
        outcome: CallOutcome,
        locally_limited: bool,
    ) -> Result<SubCallOutput, SubCallError> {
        // Public OOG includes fixed memory/operand failures that more allowance cannot fix.
        let allowance_exhausted = matches!(
            outcome.result.result,
            InstructionResult::OutOfGas
                | InstructionResult::MemoryOOG
                | InstructionResult::PrecompileOOG
                | InstructionResult::ReentrancySentryOOG
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
