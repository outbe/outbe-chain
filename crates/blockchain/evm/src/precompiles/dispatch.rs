//! Executes an admitted call and settles its gas and typed outcome.
use super::{
    call_authority::{
        is_protocol_cycle_call, metadosis_mutation_entitlements, MetadosisMutationCall,
        ResultVoteCall,
    },
    outcome::map_outbe_precompile_result,
    value_policy::{classify_boundary_value, BoundaryValue},
    OcompActivationBlockMeter,
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
use outbe_compressed_entities::ExecutionScope;
use outbe_metadosis::{api::OcompFinalizedIntentAuthority, config::OcompForkInstallV1};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::error::{PrecompileError, Result as DomainResult};
use outbe_primitives::{
    addresses::{FIDELITY_ADDRESS, METADOSIS_ADDRESS, ORACLE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS},
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
};
use revm::{
    handler::precompile_output_to_interpreter_result,
    interpreter::{CallInputs, CallScheme, InterpreterResult},
    primitives::hardfork::SpecId,
    Database,
};
use std::sync::Arc;

/// Executor-owned runtime authorities carried into one Outbe dispatch.
pub(super) struct OutbeDispatchRuntime<'a> {
    pub(super) spec: SpecId,
    pub(super) genesis_hash: B256,
    pub(super) tee_attestation_v1: &'a TeeAttestationChainSpecStateV1,
    pub(super) runtime_body_readers: Option<&'a RuntimeBodyReaders>,
    pub(super) execution_scope: &'a Arc<ExecutionScope>,
    pub(super) ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    pub(super) ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
    pub(super) ocomp_lifecycle_active: bool,
    pub(super) ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
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
    result_vote: ResultVoteCall,
    protocol_cycle_call: bool,
}

struct DispatchOutcome {
    result: DomainResult<Bytes>,
    actual_gas: u64,
}

/// Dispatch one outbe precompile call with full context access.
pub(super) fn outbe_ctx_dispatch<DB>(
    ctx: &mut EthEvmContext<DB>,
    inputs: &CallInputs,
    runtime: OutbeDispatchRuntime<'_>,
) -> Result<Option<InterpreterResult>, String>
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
    translate_outcome(outcome, runtime.runtime_body_readers, inputs.gas_limit).map(Some)
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
            return Err(PrecompileError::Revert(
                "outbe precompile: delegated call frame cannot execute a precompile".to_string(),
            ));
        }
        Ok(())
    }

    fn admit_value(&self, ocomp_lifecycle_active: bool) -> DomainResult<CallAdmission> {
        let address = self.inputs.bytecode_address;
        let caller = self.inputs.caller;
        let block_number = self.block_number;
        if caller == METADOSIS_ADDRESS && matches!(address, FIDELITY_ADDRESS | ORACLE_ADDRESS) {
            let selector = self.data.get(..4).map(alloy_primitives::hex::encode);
            tracing::warn!(
                target: "outbe::ocomp::trace",
                "OCOMP_TRACE_V1 kind=forbidden_calculation_entry block={block_number} \
                 target={address:#x} selector={}",
                selector.as_deref().unwrap_or("missing")
            );
        }
        let value = match classify_boundary_value(self.route.value_policy(), &self.inputs.value) {
            BoundaryValue::Credited(v) => v,
            BoundaryValue::Rejected(reason) => {
                return Err(PrecompileError::Revert(reason.to_string()));
            }
        };
        let result_vote = ResultVoteCall::classify(
            address,
            self.data.as_ref(),
            self.inputs.is_static,
            value,
            ocomp_lifecycle_active,
        );
        if result_vote == ResultVoteCall::WrongMode {
            return Err(outbe_metadosis::errors::result_vote_call_mode_rejection());
        }
        Ok(CallAdmission {
            value,
            result_vote,
            protocol_cycle_call: is_protocol_cycle_call(address, caller, self.data.as_ref()),
        })
    }

    fn mutation_call<'a>(
        &'a self,
        runtime: &'a OutbeDispatchRuntime<'_>,
        admission: &CallAdmission,
        cycle_active_utc_day: Option<u32>,
    ) -> MetadosisMutationCall<'a> {
        MetadosisMutationCall {
            address: self.inputs.bytecode_address,
            data: self.data.as_ref(),
            caller: self.inputs.caller,
            is_static: self.inputs.is_static,
            value: admission.value,
            ocomp_lifecycle_active: runtime.ocomp_lifecycle_active,
            result_vote: admission.result_vote,
            chain_id: self.chain_id,
            block_number: self.block_number,
            timestamp: self.timestamp,
            cycle_active_utc_day,
            preloaded_certified_state_root:
                crate::begin_block_precompile::preloaded_certified_parent_state_root(),
            ocomp_fork_install: runtime.ocomp_fork_install.as_deref(),
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
            runtime_body_readers: runtime.runtime_body_readers.cloned(),
            execution_scope: runtime.execution_scope.clone(),
            ocomp_finality_authority: runtime.ocomp_finality_authority.clone(),
            ocomp_activation_block_meter: runtime.ocomp_activation_block_meter.clone(),
            ocomp_lifecycle_active: runtime.ocomp_lifecycle_active,
            lysis_activation_entitled: admission.result_vote == ResultVoteCall::Entitled,
            metadosis_mutation_entitlements: metadosis_mutation_entitlements(
                self.mutation_call(runtime, admission, None),
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
        // Keep admission failures typed and retain the guard through execution
        // and gas settlement. Reporting and REVM translation happen afterwards.
        let result = (|| -> DomainResult<Bytes> {
            self.check_frame()?;
            let address = self.inputs.bytecode_address;
            let Some(_reentrancy) = ReentrancyStack::try_enter(address) else {
                return Err(PrecompileError::Revert(
                    "outbe precompile reentrancy denied".to_string(),
                ));
            };
            let admission = self.admit_value(runtime.ocomp_lifecycle_active)?;
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
            let result = self.dispatch_with_provider(&mut provider, runtime, &admission);
            if result.is_ok() && admission.result_vote == ResultVoteCall::Entitled {
                let block_number = self.block_number;
                let caller = self.inputs.caller;
                tracing::info!(
                    target: "outbe::ocomp::trace",
                    "OCOMP_TRACE_V1 kind=result_vote_committed block={block_number} caller={caller:#x}"
                );
            }
            let storage_gas = gas_budget.saturating_sub(provider.gas.remaining());
            actual_gas = base_gas + storage_gas;
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
        DispatchOutcome { result, actual_gas }
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
                self.mutation_call(runtime, admission, Some(active_utc_day)),
            ));
        }
        let storage = StorageHandle::new(provider);
        if admission.result_vote == ResultVoteCall::Entitled {
            outbe_metadosis::commands::submit_verified_result_vote(
                storage,
                runtime.execution_scope.as_ref(),
                self.data.as_ref(),
                admission.value,
                self.inputs.is_static,
            )
        } else if self.inputs.bytecode_address == OUTBE_SYSTEM_TX_ADDRESS {
            if let Some(readers) = runtime.runtime_body_readers {
                crate::begin_block_precompile::dispatch_with_readers_and_ocomp_install(
                    storage,
                    crate::begin_block_precompile::SystemTxRuntime {
                        scope: runtime.execution_scope.as_ref(),
                        parent: readers,
                        ocomp_fork_install: runtime.ocomp_fork_install.as_deref(),
                        tee_attestation_v1: runtime.tee_attestation_v1,
                    },
                    self.data.as_ref(),
                    self.inputs.caller,
                    admission.value,
                )
            } else {
                crate::begin_block_precompile::dispatch_with_tee_attestation(
                    storage,
                    runtime.tee_attestation_v1,
                    self.data.as_ref(),
                    self.inputs.caller,
                    admission.value,
                )
            }
        } else {
            self.route.dispatch(
                storage,
                runtime.execution_scope.as_ref(),
                runtime.runtime_body_readers,
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
    readers: Option<&RuntimeBodyReaders>,
    gas_limit: u64,
) -> Result<InterpreterResult, String> {
    if let Some(readers) = readers {
        if let Err(error) = &outcome.result {
            readers.report_precompile_error(error);
        }
    }
    match map_outbe_precompile_result(outcome.result, outcome.actual_gas) {
        Ok(output) => Ok(precompile_output_to_interpreter_result(output, gas_limit)),
        // `map_outbe_precompile_result` returns `Err` only as a revm `Fatal`.
        // `SubCall`, `Unsupported`, and every outbe variant without an explicit
        // arm reach this fatal string channel.
        Err(other) => Err(other.to_string()),
    }
}
