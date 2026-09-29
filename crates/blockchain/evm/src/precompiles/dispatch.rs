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
use alloy_primitives::{Bytes, B256};
use core::fmt::Debug;
use outbe_metadosis::{api::OcompFinalizedIntentAuthority, config::OcompForkInstallV1};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_entities::ExecutionScope;
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
    let OutbeDispatchRuntime {
        spec,
        genesis_hash,
        tee_attestation_v1,
        runtime_body_readers,
        execution_scope,
        ocomp_finality_authority,
        ocomp_activation_block_meter,
        ocomp_lifecycle_active,
        ocomp_fork_install,
    } = runtime;

    use revm::context_interface::{Block as _, ContextTr};

    let address = inputs.bytecode_address;
    let Some(route) = precompile_routes::resolve(&address) else {
        return Ok(None);
    };
    let block_number = ctx.block().number().saturating_to::<u64>();
    let chain_id = ctx.cfg().chain_id;
    let timestamp = ctx.block().timestamp().saturating_to::<u64>();

    // Materialize the exact calldata before choosing the consensus gas charge.
    // Contract -> precompile calls arrive as SharedBuffer and must pay the same
    // activation charge as top-level Bytes calls.
    let data: Bytes = inputs.input.bytes_local(ctx.local());

    // Per-precompile base gas, floored at PRECOMPILE_BASE_GAS so the
    // existing flat-cost contract still holds for default precompiles.
    let base_gas = route.base_gas(data.as_ref()).max(PRECOMPILE_BASE_GAS);
    let mut actual_gas = base_gas;
    // Keep all admission failures typed until the single REVM translation below.
    // A user-controlled error must never escape through the fatal string channel.
    let result = (|| -> DomainResult<Bytes> {
        if inputs.gas_limit < base_gas {
            return Err(PrecompileError::OutOfGas);
        }

        // A precompile's state is keyed by its own address, so a `DELEGATECALL` or
        // `CALLCODE` frame cannot give it the borrowed-code semantics those opcodes
        // promise: dispatch would read and write the precompile's own storage while
        // `caller` stays the frame's inherited caller. Any contract could then take
        // caller-authenticated actions - unstaking, voting, spending - as whoever
        // called it. Refuse the frame instead of executing it under a caller it does
        // not belong to.
        //
        // Matching the scheme rather than comparing addresses states the rule the
        // opcodes define; the address divergence those two produce is a consequence
        // of it, and one that a self-referential frame would not exhibit.
        if matches!(
            inputs.scheme,
            CallScheme::DelegateCall | CallScheme::CallCode
        ) {
            return Err(PrecompileError::Revert(
                "outbe precompile: delegated call frame cannot execute a precompile".to_string(),
            ));
        }

        // Reentrancy guard: refuse re-entry into the same outbe address on the
        // active thread's call chain.
        let Some(_reentrancy) = ReentrancyStack::try_enter(address) else {
            return Err(PrecompileError::Revert(
                "outbe precompile reentrancy denied".to_string(),
            ));
        };

        let is_static = inputs.is_static;
        let caller = inputs.caller;
        if caller == METADOSIS_ADDRESS && matches!(address, FIDELITY_ADDRESS | ORACLE_ADDRESS) {
            let selector = data.get(..4).map(alloy_primitives::hex::encode);
            tracing::warn!(
                target: "outbe::ocomp::trace",
                "OCOMP_TRACE_V1 kind=forbidden_calculation_entry block={block_number} \
                 target={address:#x} selector={}",
                selector.as_deref().unwrap_or("missing")
            );
        }
        let value = match classify_boundary_value(route.value_policy(), &inputs.value) {
            BoundaryValue::Credited(v) => v,
            BoundaryValue::Rejected(reason) => {
                return Err(PrecompileError::Revert(reason.to_string()));
            }
        };
        let result_vote = ResultVoteCall::classify(
            address,
            data.as_ref(),
            is_static,
            value,
            ocomp_lifecycle_active,
        );
        if result_vote == ResultVoteCall::WrongMode {
            return Err(outbe_metadosis::errors::result_vote_call_mode_rejection());
        }
        let gas_budget = inputs.gas_limit - base_gas;
        let gas_meter = SubcallGasMeter::new(gas_budget);
        let protocol_cycle_call = is_protocol_cycle_call(address, caller, data.as_ref());

        tracing::debug!(
            target: "outbe::precompile::gas",
            ?address,
            gas_limit = inputs.gas_limit,
            base_gas,
            gas_budget,
            "precompile dispatch entry"
        );

        let mut provider = CtxStorageProvider::new(
            ctx,
            gas_meter,
            CtxStorageProviderConfig {
                is_static,
                self_address: address,
                reentrancy_stack: ReentrancyStack,
                spec,
                genesis_hash,
                runtime_body_readers: runtime_body_readers.cloned(),
                execution_scope: execution_scope.clone(),
                ocomp_finality_authority: ocomp_finality_authority.clone(),
                ocomp_activation_block_meter: ocomp_activation_block_meter.clone(),
                ocomp_lifecycle_active,
                lysis_activation_entitled: result_vote == ResultVoteCall::Entitled,
                metadosis_mutation_entitlements: metadosis_mutation_entitlements(
                    MetadosisMutationCall {
                        address,
                        data: data.as_ref(),
                        caller,
                        is_static,
                        value,
                        ocomp_lifecycle_active,
                        result_vote,
                        chain_id,
                        block_number,
                        timestamp,
                        cycle_active_utc_day: None,
                        preloaded_certified_state_root:
                            crate::begin_block_precompile::preloaded_certified_parent_state_root(),
                        ocomp_fork_install: ocomp_fork_install.as_deref(),
                    },
                ),
            },
        );
        // Probe failures must still settle the provider's consumed gas and reach
        // the same error reporting and outcome mapping as command failures.
        let result = (|| -> DomainResult<Bytes> {
            if protocol_cycle_call {
                let active_utc_day = {
                    let storage = StorageHandle::new(&mut provider);
                    storage
                        .contract::<outbe_cycle::schema::Cycle<'_>>()
                        .active_utc_day
                        .read()?
                };
                provider.replace_metadosis_mutation_entitlements(metadosis_mutation_entitlements(
                    MetadosisMutationCall {
                        address,
                        data: data.as_ref(),
                        caller,
                        is_static,
                        value,
                        ocomp_lifecycle_active,
                        result_vote,
                        chain_id,
                        block_number,
                        timestamp,
                        cycle_active_utc_day: Some(active_utc_day),
                        preloaded_certified_state_root:
                            crate::begin_block_precompile::preloaded_certified_parent_state_root(),
                        ocomp_fork_install: ocomp_fork_install.as_deref(),
                    },
                ));
            }
            let storage = StorageHandle::new(&mut provider);
            if result_vote == ResultVoteCall::Entitled {
                outbe_metadosis::commands::submit_verified_result_vote(
                    storage,
                    execution_scope.as_ref(),
                    data.as_ref(),
                    value,
                    is_static,
                )
            } else if address == OUTBE_SYSTEM_TX_ADDRESS {
                if let Some(readers) = runtime_body_readers {
                    crate::begin_block_precompile::dispatch_with_readers_and_ocomp_install(
                        storage,
                        crate::begin_block_precompile::SystemTxRuntime {
                            scope: execution_scope.as_ref(),
                            parent: readers,
                            ocomp_fork_install: ocomp_fork_install.as_deref(),
                            tee_attestation_v1,
                        },
                        data.as_ref(),
                        caller,
                        value,
                    )
                } else {
                    crate::begin_block_precompile::dispatch_with_tee_attestation(
                        storage,
                        tee_attestation_v1,
                        data.as_ref(),
                        caller,
                        value,
                    )
                }
            } else {
                route.dispatch(
                    storage,
                    execution_scope.as_ref(),
                    runtime_body_readers,
                    precompile_routes::RouteCall {
                        callee: address,
                        data: data.as_ref(),
                        caller,
                        value,
                    },
                )
            }
        })();
        if result.is_ok() && result_vote == ResultVoteCall::Entitled {
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

    if let Some(readers) = runtime_body_readers {
        if let Err(error) = &result {
            readers.report_precompile_error(error);
        }
    }

    let precompile_result = map_outbe_precompile_result(result, actual_gas);

    let interp_result = match precompile_result {
        Ok(precompile_output) => {
            precompile_output_to_interpreter_result(precompile_output, inputs.gas_limit)
        }
        // Both Fatal(String) and FatalAny(_) propagate as Err(String). At
        // revm 38 these are the only variants; the wildcard is defensive.
        Err(other) => return Err(other.to_string()),
    };

    Ok(Some(interp_result))
}
