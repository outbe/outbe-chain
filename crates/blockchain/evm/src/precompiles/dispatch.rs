//! Executes an admitted call and settles its gas and typed outcome.
use super::{
    call_authority::{precompile_mutation_entitlements, PrecompileMutationCall},
    outcome::map_outbe_precompile_result,
    routes,
    value_policy::{classify_boundary_value, BoundaryCallValue, BoundaryValue},
    ExecutionAbortBridge,
};
use crate::{
    factory::OutbeEvmTypes,
    gas::SubcallGasMeter,
    storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack},
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};
use alloy_primitives::{Bytes, B256, U256};
use evm2::{
    interpreter::{GasTracker, Message, MessageKind},
    precompiles::{PrecompileError as EvmPrecompileError, PrecompileHalt, PrecompileResult},
    Evm, SpecId,
};
use outbe_primitives::{
    addresses::OUTBE_SYSTEM_TX_ADDRESS,
    error::{PrecompileError, Result as DomainResult},
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
};

/// Executor-owned runtime authorities carried into one Outbe dispatch.
pub(super) struct OutbeDispatchRuntime<'a> {
    pub(super) abort_bridge: &'a ExecutionAbortBridge,
    pub(super) spec: SpecId,
    pub(super) genesis_hash: B256,
    pub(super) genesis_timestamp: u64,
    pub(super) tee_attestation_v1: &'a TeeAttestationChainSpecStateV1,
    pub(super) host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
}

/// Captures the frame and consensus inputs before borrowing the EVM for storage.
struct DispatchCall<'a> {
    message: &'a Message<OutbeEvmTypes>,
    route: routes::Route,
    host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
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

#[cfg(test)]
std::thread_local! {
    pub(super) static PANIC_WITH_NATIVE_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl DispatchOutcome {
    fn panicked(
        runtime: &OutbeDispatchRuntime<'_>,
        entry_depth: usize,
        depth_after: usize,
    ) -> Self {
        // A Halt would clear an already-latched node-local abort in translate_outcome.
        let error = if let Err(error) = runtime.abort_bridge.check_subcall() {
            PrecompileError::SubCall(error)
        } else if depth_after != entry_depth {
            PrecompileError::Fatal("precompile subcall unwound with dirty journal depth".into())
        } else {
            PrecompileError::Halt(PrecompileHalt::Other(std::borrow::Cow::Borrowed(
                "precompile dispatch panicked",
            )))
        };
        Self {
            result: Err(error),
            actual_gas: 0,
            refund: 0,
        }
    }
}

/// Dispatch one outbe precompile call with full safe access to [`evm2::Evm`].
pub(super) fn outbe_ctx_dispatch(
    evm: &mut Evm<'_, OutbeEvmTypes>,
    message: &Message<OutbeEvmTypes>,
    gas: &mut GasTracker,
    runtime: OutbeDispatchRuntime<'_>,
) -> Option<PrecompileResult> {
    let route = routes::resolve(runtime.host.as_ref(), &message.code_address)?;
    let block_number = evm.block().number.saturating_to::<u64>();
    let chain_id = evm.version().chain_id;
    // Materialize the exact calldata safely via `evm.message_input(message)` (Reth #28413)
    // before choosing the consensus gas charge.
    let data = Bytes::copy_from_slice(evm.message_input(message));
    let entry_depth = evm.ext().native_frame_depth;
    let prev_call_depth = evm.ext().current_call_depth;
    evm.ext_mut().current_call_depth = message.depth;

    // Every provider and storage guard must unwind before inspecting native frame depth.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        PANIC_WITH_NATIVE_FRAME.with(|armed| {
            if armed.replace(false) {
                evm.ext_mut().native_frame_depth += 1;
                panic!("secret native frame panic");
            }
        });
        let base_gas = route
            .base_gas(runtime.host.as_ref(), data.as_ref())
            .max(PRECOMPILE_BASE_GAS);
        let timestamp = match outbe_primitives::time::timestamp_secs(evm.block().timestamp) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                return DispatchOutcome {
                    result: Err(if message.gas_limit < base_gas {
                        PrecompileError::OutOfGas
                    } else {
                        error.into()
                    }),
                    actual_gas: base_gas,
                    refund: 0,
                };
            }
        };
        let call = DispatchCall {
            message,
            route,
            host: std::sync::Arc::clone(&runtime.host),
            data,
            block_number,
            chain_id,
            timestamp,
            base_gas,
        };
        call.execute(evm, &runtime)
    }))
    .unwrap_or_else(|_| {
        let depth_after = evm.ext().native_frame_depth;
        tracing::error!(
            target: "outbe::precompile",
            address = ?message.code_address,
            caller = ?message.caller,
            block_number,
            entry_depth,
            depth_after,
            "precompile dispatch panicked"
        );
        DispatchOutcome::panicked(&runtime, entry_depth, depth_after)
    });
    evm.ext_mut().current_call_depth = prev_call_depth;

    // A domain-local catch must not hide a leaked native frame on its success path.
    let depth_after = evm.ext().native_frame_depth;
    let outcome = if depth_after != entry_depth {
        DispatchOutcome::panicked(&runtime, entry_depth, depth_after)
    } else {
        outcome
    };
    Some(translate_outcome(outcome, &runtime, gas))
}

impl DispatchCall<'_> {
    fn check_frame(&self) -> DomainResult<()> {
        if self.message.gas_limit < self.base_gas {
            return Err(PrecompileError::OutOfGas);
        }
        // Borrowed-code frames cannot use precompile storage with an inherited
        // caller: that would allow caller-authenticated actions on its behalf.
        // Match the opcode kind, including self-referential borrowed frames.
        if matches!(
            self.message.kind,
            MessageKind::DelegateCall | MessageKind::CallCode
        ) {
            return Err(crate::errors::OutbePrecompileDispatchError::DelegatedCallFrame.into());
        }
        Ok(())
    }

    fn admit_value(&self) -> DomainResult<CallAdmission> {
        let address = self.message.code_address;
        let caller = self.message.caller;
        let boundary_value = BoundaryCallValue::from_message(self.message);
        let value = match classify_boundary_value(
            self.route.value_policy(self.host.as_ref()),
            &boundary_value,
        ) {
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
            protocol_cycle_call: self.host.is_protocol_cycle_call(
                address,
                caller,
                self.data.as_ref(),
            ),
        })
    }

    fn mutation_call<'a>(
        &'a self,
        admission: &CallAdmission,
        cycle_active_utc_day: Option<u32>,
    ) -> PrecompileMutationCall<'a> {
        PrecompileMutationCall {
            address: self.message.code_address,
            data: self.data.as_ref(),
            caller: self.message.caller,
            is_static: self.message.is_static(),
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
        let address = self.message.code_address;
        CtxStorageProviderConfig {
            is_static: self.message.is_static(),
            self_address: address,
            reentrancy_stack: ReentrancyStack,
            spec: runtime.spec,
            genesis_hash: runtime.genesis_hash,
            genesis_timestamp: 0,
            abort_bridge: runtime.abort_bridge.clone(),
            scoped_mutation_entitlements: precompile_mutation_entitlements(
                self.host.as_ref(),
                self.mutation_call(admission, None),
            ),
        }
        .with_genesis_timestamp(runtime.genesis_timestamp)
    }

    fn execute(
        &self,
        evm: &mut Evm<'_, OutbeEvmTypes>,
        runtime: &OutbeDispatchRuntime<'_>,
    ) -> DispatchOutcome {
        let mut actual_gas = self.base_gas;
        let mut refund = 0;
        // Keep admission failures typed and retain the guard through execution
        // and gas settlement. Reporting and EVM translation happen afterwards.
        let result = (|| -> DomainResult<Bytes> {
            self.check_frame()?;
            let address = self.message.code_address;
            let Some(_reentrancy) = ReentrancyStack::try_enter(address) else {
                return Err(crate::errors::OutbePrecompileDispatchError::ReentrancyDenied.into());
            };
            let admission = self.admit_value()?;
            let gas_budget = self.message.gas_limit - self.base_gas;
            let gas_meter = SubcallGasMeter::new(gas_budget);
            let base_gas = self.base_gas;
            tracing::debug!(
                target: "outbe::precompile::gas",
                ?address,
                gas_limit = self.message.gas_limit,
                base_gas,
                gas_budget,
                "precompile dispatch entry"
            );
            let mut provider = CtxStorageProvider::new(
                evm,
                gas_meter,
                self.provider_config(runtime, &admission),
                std::sync::Arc::clone(&self.host),
            );
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

    fn dispatch_with_provider(
        &self,
        provider: &mut CtxStorageProvider<'_, '_>,
        runtime: &OutbeDispatchRuntime<'_>,
        admission: &CallAdmission,
    ) -> DomainResult<Bytes> {
        if admission.protocol_cycle_call {
            let active_utc_day = {
                let storage = StorageHandle::new(provider);
                self.host.derive_active_utc_day(
                    storage,
                    runtime.genesis_timestamp,
                    self.timestamp,
                )?
            };
            provider.replace_scoped_mutation_entitlements(precompile_mutation_entitlements(
                self.host.as_ref(),
                self.mutation_call(admission, Some(active_utc_day)),
            ));
        }
        let storage = StorageHandle::new(provider);
        if self.message.code_address == OUTBE_SYSTEM_TX_ADDRESS {
            self.host.dispatch_system_tx(
                storage,
                self.data.as_ref(),
                self.message.caller,
                admission.value,
                runtime.tee_attestation_v1,
            )
        } else {
            self.route.dispatch(
                self.host.as_ref(),
                storage,
                routes::RouteCall {
                    callee: self.message.code_address,
                    data: self.data.as_ref(),
                    caller: self.message.caller,
                    value: admission.value,
                },
            )
        }
    }
}

fn translate_outcome(
    outcome: DispatchOutcome,
    runtime: &OutbeDispatchRuntime<'_>,
    gas: &mut GasTracker,
) -> PrecompileResult {
    if let Err(error) = &outcome.result {
        runtime.abort_bridge.observe(error);
    }
    match map_outbe_precompile_result(outcome.result) {
        Ok(output) => {
            if gas.try_spend(outcome.actual_gas).is_err() {
                gas.spend_all();
                Err(EvmPrecompileError::Halt(PrecompileHalt::OutOfGas))
            } else {
                gas.record_refund(outcome.refund);
                Ok(output)
            }
        }
        Err(EvmPrecompileError::Revert(bytes)) => {
            if gas.try_spend(outcome.actual_gas).is_err() {
                gas.spend_all();
                Err(EvmPrecompileError::Halt(PrecompileHalt::OutOfGas))
            } else {
                Err(EvmPrecompileError::Revert(bytes))
            }
        }
        Err(EvmPrecompileError::Halt(halt)) => {
            gas.spend_all();
            Err(EvmPrecompileError::Halt(halt))
        }
        // Fatal and Database errors MUST NEVER be swallowed into a Revert or Halt.
        Err(fatal @ (EvmPrecompileError::Fatal(_) | EvmPrecompileError::Database(_))) => Err(fatal),
    }
}
