//! Executes an admitted call and settles its gas and typed outcome.
use super::{
    call_authority::{
        is_protocol_cycle_call, metadosis_mutation_entitlements, MetadosisMutationCall,
    },
    outcome::map_outbe_precompile_result_with_refund,
    value_policy::{classify_boundary_value, BoundaryValue},
    ExecutionAbortBridge,
};
use crate::{
    gas::SubcallGasMeter,
    precompile_routes,
    storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack},
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};
use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Bytes, B256, U256};
use core::fmt::Debug;
use outbe_primitives::error::{PrecompileError, Result as DomainResult};
use outbe_primitives::{
    addresses::OUTBE_SYSTEM_TX_ADDRESS,
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
};
use revm::{
    handler::precompile_output_to_interpreter_result,
    interpreter::{CallInputs, CallScheme, InterpreterResult},
    primitives::hardfork::SpecId,
    Database,
};

/// Executor-owned runtime authorities carried into one Outbe dispatch.
pub(super) struct OutbeDispatchRuntime<'a> {
    pub(super) abort_bridge: &'a ExecutionAbortBridge,
    pub(super) spec: SpecId,
    pub(super) genesis_hash: B256,
    pub(super) tee_attestation_v1: &'a TeeAttestationChainSpecStateV1,
}

/// Captures the frame and consensus inputs before borrowing the context for storage.
struct DispatchCall<'a> {
    inputs: &'a CallInputs,
    route: precompile_routes::Route,
    data: Bytes,
    block_number: u64,
    chain_id: u64,
    timestamp: u64,
    base_gas: u64,
}

struct CallAdmission {
    value: U256,
    protocol_cycle_call: bool,
}

struct DispatchOutcome {
    result: DomainResult<Bytes>,
    actual_gas: u64,
    refund: i64,
}

/// Dispatch one outbe precompile call with full context access.
pub(super) fn outbe_ctx_dispatch<DB>(
    ctx: &mut EthEvmContext<DB>,
    inputs: &CallInputs,
    runtime: OutbeDispatchRuntime<'_>,
) -> Result<Option<InterpreterResult>, crate::errors::OutbePrecompileFatalError>
where
    DB: Database + Debug,
    DB::Error: Debug,
{
    use revm::context_interface::{Block as _, ContextTr};

    let Some(route) = precompile_routes::resolve(&inputs.bytecode_address) else {
        return Ok(None);
    };
    let block_number = ctx.block().number().saturating_to::<u64>();
    let chain_id = ctx.cfg().chain_id;
    let timestamp = ctx.block().timestamp().saturating_to::<u64>();
    // Materialize the exact calldata before choosing the consensus gas charge.
    // SharedBuffer and top-level Bytes calls must pay the same activation charge.
    let data: Bytes = inputs.input.bytes_local(ctx.local());
    let base_gas = route.base_gas(data.as_ref()).max(PRECOMPILE_BASE_GAS);
    let call = DispatchCall {
        inputs,
        route,
        data,
        block_number,
        chain_id,
        timestamp,
        base_gas,
    };
    let outcome = call.execute(ctx, &runtime);
    Ok(Some(translate_outcome(
        outcome,
        &runtime,
        inputs.gas_limit,
    )?))
}

impl DispatchCall<'_> {
    fn check_frame(&self) -> DomainResult<()> {
        if self.inputs.gas_limit < self.base_gas {
            return Err(PrecompileError::OutOfGas);
        }
        // Borrowed-code frames cannot use precompile storage with an inherited
        // caller: that would allow caller-authenticated actions on its behalf.
        // Match the opcode scheme, including self-referential borrowed frames.
        if matches!(
            self.inputs.scheme,
            CallScheme::DelegateCall | CallScheme::CallCode
        ) {
            return Err(crate::errors::OutbePrecompileDispatchError::DelegatedCallFrame.into());
        }
        Ok(())
    }

    fn admit_value(&self) -> DomainResult<CallAdmission> {
        let address = self.inputs.bytecode_address;
        let caller = self.inputs.caller;
        let value = match classify_boundary_value(self.route.value_policy(), &self.inputs.value) {
            BoundaryValue::Credited(v) => v,
            BoundaryValue::Rejected(reason) => {
                return Err(
                    crate::errors::OutbePrecompileDispatchError::RejectedBoundaryValue(reason)
                        .into(),
                );
            }
        };
        Ok(CallAdmission {
            value,
            protocol_cycle_call: is_protocol_cycle_call(address, caller, self.data.as_ref()),
        })
    }

    fn mutation_call<'a>(
        &'a self,
        admission: &CallAdmission,
        cycle_active_utc_day: Option<u32>,
    ) -> MetadosisMutationCall<'a> {
        MetadosisMutationCall {
            address: self.inputs.bytecode_address,
            data: self.data.as_ref(),
            caller: self.inputs.caller,
            is_static: self.inputs.is_static,
            value: admission.value,
            chain_id: self.chain_id,
            block_number: self.block_number,
            timestamp: self.timestamp,
            cycle_active_utc_day,
        }
    }

    fn provider_config(
        &self,
        runtime: &OutbeDispatchRuntime<'_>,
        admission: &CallAdmission,
    ) -> CtxStorageProviderConfig {
        let address = self.inputs.bytecode_address;
        CtxStorageProviderConfig {
            is_static: self.inputs.is_static,
            self_address: address,
            reentrancy_stack: ReentrancyStack,
            spec: runtime.spec,
            genesis_hash: runtime.genesis_hash,

            abort_bridge: runtime.abort_bridge.clone(),

            metadosis_mutation_entitlements: metadosis_mutation_entitlements(
                self.mutation_call(admission, None),
            ),
        }
    }

    fn execute<DB>(
        &self,
        ctx: &mut EthEvmContext<DB>,
        runtime: &OutbeDispatchRuntime<'_>,
    ) -> DispatchOutcome
    where
        DB: Database + Debug,
        DB::Error: Debug,
    {
        let mut actual_gas = self.base_gas;
        let mut refund = 0;
        // Keep admission failures typed and retain the guard through execution
        // and gas settlement. Reporting and REVM translation happen afterwards.
        let result = (|| -> DomainResult<Bytes> {
            self.check_frame()?;
            let address = self.inputs.bytecode_address;
            let Some(_reentrancy) = ReentrancyStack::try_enter(address) else {
                return Err(crate::errors::OutbePrecompileDispatchError::ReentrancyDenied.into());
            };
            let admission = self.admit_value()?;
            let gas_budget = self.inputs.gas_limit - self.base_gas;
            let gas_meter = SubcallGasMeter::new(gas_budget);
            let base_gas = self.base_gas;
            tracing::debug!(
                target: "outbe::precompile::gas",
                ?address,
                gas_limit = self.inputs.gas_limit,
                base_gas,
                gas_budget,
                "precompile dispatch entry"
            );
            let mut provider =
                CtxStorageProvider::new(ctx, gas_meter, self.provider_config(runtime, &admission));
            // A failing probe or command must still settle consumed provider gas.
            let mut result = self.dispatch_with_provider(&mut provider, runtime, &admission);
            if provider.subcall_out_of_gas {
                // A handler cannot catch parent OOG and report success/revert.
                // Preserve any subsequent infrastructure failure: it must still
                // abort execution instead of becoming a user receipt.
                result = match result {
                    Ok(_) => Err(PrecompileError::OutOfGas),
                    Err(error) if crate::errors::is_overridden_by_subcall_out_of_gas(&error) => {
                        Err(PrecompileError::OutOfGas)
                    }
                    other => other,
                };
            }
            let storage_gas = gas_budget.saturating_sub(provider.gas.remaining());
            actual_gas = base_gas + storage_gas;
            if result.is_ok() {
                refund = provider.gas.refunded();
            }
            tracing::debug!(
                target: "outbe::precompile::gas",
                ?address,
                storage_gas,
                actual_gas,
                gas_remaining = provider.gas.remaining(),
                is_err = result.is_err(),
                "precompile dispatch exit"
            );
            result
        })();
        DispatchOutcome {
            result,
            actual_gas,
            refund,
        }
    }

    fn dispatch_with_provider<DB>(
        &self,
        provider: &mut CtxStorageProvider<'_, DB>,
        runtime: &OutbeDispatchRuntime<'_>,
        admission: &CallAdmission,
    ) -> DomainResult<Bytes>
    where
        DB: Database + Debug,
        DB::Error: Debug,
    {
        if admission.protocol_cycle_call {
            let active_utc_day = {
                let storage = StorageHandle::new(provider);
                storage
                    .contract::<outbe_cycle::schema::Cycle<'_>>()
                    .active_utc_day
                    .read()?
            };
            provider.replace_metadosis_mutation_entitlements(metadosis_mutation_entitlements(
                self.mutation_call(admission, Some(active_utc_day)),
            ));
        }
        let storage = StorageHandle::new(provider);
        if self.inputs.bytecode_address == OUTBE_SYSTEM_TX_ADDRESS {
            crate::begin_block_precompile::dispatch_with_runtime(
                storage,
                crate::begin_block_precompile::SystemTxRuntime {
                    tee_attestation_v1: runtime.tee_attestation_v1,
                },
                self.data.as_ref(),
                self.inputs.caller,
                admission.value,
            )
        } else {
            self.route.dispatch(
                storage,
                precompile_routes::RouteCall {
                    callee: self.inputs.bytecode_address,
                    data: self.data.as_ref(),
                    caller: self.inputs.caller,
                    value: admission.value,
                },
            )
        }
    }
}

fn translate_outcome(
    outcome: DispatchOutcome,
    runtime: &OutbeDispatchRuntime<'_>,
    gas_limit: u64,
) -> Result<InterpreterResult, crate::errors::OutbePrecompileFatalError> {
    if let Err(error) = &outcome.result {
        runtime.abort_bridge.observe(error);
    }
    match map_outbe_precompile_result_with_refund(
        outcome.result,
        outcome.actual_gas,
        outcome.refund,
    ) {
        Ok(output) => Ok(precompile_output_to_interpreter_result(output, gas_limit)),
        // `map_outbe_precompile_result` returns `Err` only as a revm `Fatal`.
        // Provider `SubCall`, `Unsupported`, and every outbe variant without an explicit
        // arm reach this fatal string channel.
        Err(other) => Err(crate::errors::OutbePrecompileFatalError::from_revm(other)),
    }
}
