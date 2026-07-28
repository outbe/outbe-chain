//! Outbe precompile registration.
//!
//! Routes outbe stateful precompile addresses through
//! `PrecompilesMap::set_ctx_dispatch_hook` (outbe fork extension on alloy-evm)
//! so the dispatch closure receives a raw pointer to the unbroken
//! `&mut EthEvmContext<DB>`. The closure casts the pointer back to the
//! concrete context type for the current `DB`, builds a [`CtxStorageProvider`]
//! borrowing that context, and dispatches via [`StorageHandle`]. Sub-call
//! invocations from the precompile body hand the same `&mut ctx` to the
//! sub-call driver in [`crate::sub_call`].

use alloy_evm::{eth::EthEvmContext, precompiles::PrecompilesMap};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{Revert, SolError};
use core::fmt::Debug;
use core::marker::PhantomData;
use outbe_compressed_entities::ExecutionScope;
use outbe_metadosis::ocomp::activation::OcompFinalizedIntentAuthority;
use outbe_metadosis::ocomp::fork::OcompForkInstallV1;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::addresses::{
    FIDELITY_ADDRESS, METADOSIS_ADDRESS, ORACLE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS,
};
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
use outbe_primitives::storage::StorageHandle;
use revm::{
    handler::{precompile_output_to_interpreter_result, EthPrecompiles, PrecompileProvider},
    interpreter::{CallInputs, InterpreterResult},
    precompile::{PrecompileHalt, PrecompileOutput, PrecompileResult},
    primitives::hardfork::SpecId,
    Database,
};
use std::sync::Arc;

use crate::{
    gas::SubcallGasMeter,
    precompile_routes,
    storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack},
};

/// Shared marker retained in the sub-call context while q-forming apply
/// accounting is migrated to the direct result-vote path.
#[derive(Debug, Default)]
pub struct OcompActivationBlockMeter;

/// ABI-encode a revert reason as the Solidity-standard `Error(string)`
/// (selector `0x08c379a0` followed by `abi.encode(reason)`).
fn encode_revert_reason(msg: String) -> Bytes {
    Bytes::from(Revert::from(msg).abi_encode())
}

/// Translate the outbe-level [`outbe_primitives::error::PrecompileError`] (the
/// flat error type returned from every outbe precompile dispatch function)
/// into a revm [`PrecompileResult`] that the EVM interpreter understands.
///
/// `actual_gas` is the total gas charge attributed to this precompile call
/// (`PRECOMPILE_BASE_GAS` plus any storage-op gas). It is reported on
/// success and `Revert*` paths so the interpreter charges the caller
/// correctly; `Halt(OOG)` reports zero gas because revm treats OOG halts
/// as "consume everything" via `spend_all` in
/// `revm-handler::precompile_output_to_interpreter_result`.
///
/// The mapping is exhaustive over `PrecompileError`'s declared variants;
/// the trailing wildcard arm exists only to satisfy `#[non_exhaustive]`
/// from outbe-primitives and surfaces unknown variants as `Fatal` rather
/// than panicking. refine the `SubCall(_)`
/// arm to per-variant halt mappings once the sub-call body produces those
/// errors at runtime.
#[doc(hidden)]
pub fn map_outbe_precompile_result(
    result: outbe_primitives::error::Result<Bytes>,
    actual_gas: u64,
) -> PrecompileResult {
    match result {
        Ok(bytes) => Ok(PrecompileOutput::new(actual_gas, bytes, 0)),
        Err(outbe_primitives::error::PrecompileError::OutOfGas) => {
            Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0))
        }
        Err(outbe_primitives::error::PrecompileError::Revert(msg)) => Ok(PrecompileOutput::revert(
            actual_gas,
            encode_revert_reason(msg),
            0,
        )),
        Err(outbe_primitives::error::PrecompileError::RevertBytes(bytes)) => {
            Ok(PrecompileOutput::revert(actual_gas, bytes, 0))
        }
        Err(outbe_primitives::error::PrecompileError::WriteProtection) => Ok(
            PrecompileOutput::halt(PrecompileHalt::other("state change during static call"), 0),
        ),
        Err(outbe_primitives::error::PrecompileError::SubCall(err)) => Err(
            revm::precompile::PrecompileError::Fatal(format!("sub-call error: {err:?}")),
        ),
        Err(outbe_primitives::error::PrecompileError::Unsupported) => Err(
            revm::precompile::PrecompileError::Fatal("precompile reported Unsupported".to_string()),
        ),
        Err(e) => Err(revm::precompile::PrecompileError::Fatal(e.to_string())),
    }
}

fn is_lysis_result_vote_call(address: Address, data: &[u8], is_static: bool, value: U256) -> bool {
    address == outbe_ocomp_protocol::abi::METADOSIS_ADDRESS
        && data.get(..4).is_some_and(|selector| {
            selector == outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR
        })
        && !is_static
        && value.is_zero()
}

/// Returns the list of outbe precompile addresses registered by
/// [`extend_outbe_precompiles`].
///
/// Lookup and enumeration are generated from the same compact declaration in
/// [`crate::precompile_routes`], so dispatch-recognized exact routes cannot be omitted
/// from this list.
pub fn outbe_precompile_addresses() -> &'static [Address] {
    precompile_routes::EXACT_ADDRESSES
}

/// Register outbe stateful precompile dispatch on the given [`PrecompilesMap`]
/// via the `set_ctx_dispatch_hook` fork extension.
///
/// The hook receives a raw pointer to the unbroken `&mut EthEvmContext<DB>`
/// before revm destructures it into `EvmInternals`. The dispatch closure
/// casts the pointer back to `&mut EthEvmContext<DB>` (safe because the
/// `EvmFactory` impl that called us is specialised for the same DB), builds a
/// [`CtxStorageProvider`] borrowing that context, and dispatches the outbe
/// precompile through a [`StorageHandle`]. Sub-call from precompile body
/// reaches `sub_call::run` through the provider's `sub_call` method.
/// Registers Outbe precompiles with an executor-owned compressed-entity scope.
pub fn extend_outbe_precompiles<DB>(
    precompiles: &mut PrecompilesMap,
    spec: SpecId,
    runtime_body_readers: Option<RuntimeBodyReaders>,
    execution_scope: Arc<ExecutionScope>,
    ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    ocomp_lifecycle_active: bool,
    ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
) where
    DB: Database + Debug,
    DB::Error: Debug,
{
    let ocomp_activation_block_meter = Arc::new(OcompActivationBlockMeter);
    precompiles.set_ctx_dispatch_hook(
        // handles: claim every outbe address.
        |addr: &Address| precompile_routes::resolve(addr).is_some(),
        // dispatch: ctx_ptr is `*mut EthEvmContext<DB>` (cast in our caller, see
        // `PrecompileProvider::run` in the fork's `precompiles.rs`).
        move |ctx_ptr, inputs| {
            #[allow(unsafe_code)] // sole audited unsafe site; justified below.
            // SAFETY: alloy-evm fork's `PrecompileProvider::run` for
            // PrecompilesMap (specialised at impl site for our `Context<DB>`)
            // casts `&mut Context<...>` to `*mut c_void` and feeds it here.
            // The `DB` generic of this closure is the same `DB` of the
            // `Context<...>` the impl is specialised for (set at
            // `OutbeEvmFactory::create_evm<DB>` call site).
            let ctx: &mut EthEvmContext<DB> = unsafe { &mut *(ctx_ptr as *mut _) };
            outbe_ctx_dispatch::<DB>(
                ctx,
                inputs,
                OutbeDispatchRuntime {
                    spec,
                    runtime_body_readers: runtime_body_readers.as_ref(),
                    execution_scope: &execution_scope,
                    ocomp_finality_authority: ocomp_finality_authority.clone(),
                    ocomp_activation_block_meter: ocomp_activation_block_meter.clone(),
                    ocomp_lifecycle_active,
                    ocomp_fork_install: ocomp_fork_install.clone(),
                },
            )
        },
    );
}

/// Executor-owned runtime authorities carried into one Outbe dispatch.
struct OutbeDispatchRuntime<'a> {
    spec: SpecId,
    runtime_body_readers: Option<&'a RuntimeBodyReaders>,
    execution_scope: &'a Arc<ExecutionScope>,
    ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
    ocomp_lifecycle_active: bool,
    ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
}

/// Dispatch one outbe precompile call with full context access.
fn outbe_ctx_dispatch<DB>(
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

    // Materialize the exact calldata before choosing the consensus gas charge.
    // Contract -> precompile calls arrive as SharedBuffer and must pay the same
    // activation charge as top-level Bytes calls.
    let data: Bytes = inputs.input.bytes_local(ctx.local());

    // Per-precompile base gas, floored at PRECOMPILE_BASE_GAS so the
    // existing flat-cost contract still holds for default precompiles.
    let is_active_result_vote_selector = ocomp_lifecycle_active
        && address == METADOSIS_ADDRESS
        && data.get(..4).is_some_and(|selector| {
            selector == outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR
        });
    let base_gas = route.base_gas(data.as_ref()).max(PRECOMPILE_BASE_GAS);
    if inputs.gas_limit < base_gas {
        let out = PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0);
        return Ok(Some(precompile_output_to_interpreter_result(
            out,
            inputs.gas_limit,
        )));
    }

    // Reentrancy guard: refuse re-entry into the same outbe address on the
    // active thread's call chain.
    let Some(_reentrancy) = ReentrancyStack::try_enter(address) else {
        let out = PrecompileOutput::revert(
            base_gas,
            encode_revert_reason("outbe precompile reentrancy denied".to_string()),
            0,
        );
        return Ok(Some(precompile_output_to_interpreter_result(
            out,
            inputs.gas_limit,
        )));
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
    let value = match inputs.value {
        revm::interpreter::CallValue::Transfer(v) => v,
        revm::interpreter::CallValue::Apparent(v) => v,
    };
    let gas_budget = inputs.gas_limit - base_gas;
    let gas_meter = SubcallGasMeter::new(gas_budget);

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
            runtime_body_readers: runtime_body_readers.cloned(),
            execution_scope: execution_scope.clone(),
            ocomp_finality_authority: ocomp_finality_authority.clone(),
            ocomp_activation_block_meter: ocomp_activation_block_meter.clone(),
            ocomp_lifecycle_active,
            lysis_activation_entitled: is_lysis_result_vote_call(
                address,
                data.as_ref(),
                is_static,
                value,
            ) && ocomp_lifecycle_active,
        },
    );
    let storage = StorageHandle::new(&mut provider);
    let result = if is_active_result_vote_selector {
        outbe_metadosis::ocomp::vote::dispatch_public_result_vote(
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
                execution_scope.as_ref(),
                readers,
                ocomp_fork_install.as_deref(),
                data.as_ref(),
                caller,
                value,
            )
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
    };
    if result.is_ok() && is_active_result_vote_selector {
        tracing::info!(
            target: "outbe::ocomp::trace",
            "OCOMP_TRACE_V1 kind=result_vote_committed block={block_number} caller={caller:#x}"
        );
    }

    if let Some(readers) = runtime_body_readers {
        if let Err(error) = &result {
            readers.report_precompile_error(error);
        }
    }

    let storage_gas = gas_budget.saturating_sub(provider.gas.remaining());
    let actual_gas = base_gas + storage_gas;

    tracing::debug!(
        target: "outbe::precompile::gas",
        ?address,
        storage_gas,
        actual_gas,
        gas_remaining = provider.gas.remaining(),
        is_err = result.is_err(),
        "precompile dispatch exit"
    );

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

/// Precompile provider for the borrow-mode sub-call `Evm`
/// (`CTX = &mut EthEvmContext<DB>`), used by [`crate::sub_call`].
///
/// Mirrors the top-level [`PrecompilesMap`] semantics so a sub-call to any
/// outbe precompile behaves exactly like a top-level call: outbe stateful
/// precompiles dispatch through [`outbe_ctx_dispatch`], and everything else
/// (Ethereum precompiles `0x01..0x0a`, ordinary contract calls) falls back to
/// the standard [`EthPrecompiles`].
pub(crate) struct OutbeSubCallPrecompiles<DB> {
    /// Fallback provider for the Ethereum precompiles `0x01..0x0a`.
    eth: EthPrecompiles,
    /// EVM spec id, forwarded to [`outbe_ctx_dispatch`].
    spec: SpecId,
    runtime_body_readers: Option<RuntimeBodyReaders>,
    execution_scope: Arc<ExecutionScope>,
    ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
    ocomp_lifecycle_active: bool,
    _db: PhantomData<fn() -> DB>,
}

impl<DB> OutbeSubCallPrecompiles<DB> {
    pub(crate) fn new(
        spec: SpecId,
        runtime_body_readers: Option<RuntimeBodyReaders>,
        execution_scope: Arc<ExecutionScope>,
        ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
        ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
        ocomp_lifecycle_active: bool,
    ) -> Self {
        Self {
            eth: EthPrecompiles::new(spec),
            spec,
            runtime_body_readers,
            execution_scope,
            ocomp_finality_authority,
            ocomp_activation_block_meter,
            ocomp_lifecycle_active,
            _db: PhantomData,
        }
    }
}

impl<DB> PrecompileProvider<&mut EthEvmContext<DB>> for OutbeSubCallPrecompiles<DB>
where
    DB: Database + Debug,
    DB::Error: Debug,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: SpecId) -> bool {
        self.spec = spec;
        <EthPrecompiles as PrecompileProvider<&mut EthEvmContext<DB>>>::set_spec(
            &mut self.eth,
            spec,
        )
    }

    fn run(
        &mut self,
        context: &mut &mut EthEvmContext<DB>,
        inputs: &CallInputs,
    ) -> Result<Option<InterpreterResult>, String> {
        // Outbe stateful precompiles first. `outbe_ctx_dispatch` returns
        // `Ok(None)` for any non-outbe address, so this is a cheap no-op for
        // Ethereum precompiles and ordinary contract targets.
        if let Some(result) = outbe_ctx_dispatch::<DB>(
            &mut **context,
            inputs,
            OutbeDispatchRuntime {
                spec: self.spec,
                runtime_body_readers: self.runtime_body_readers.as_ref(),
                execution_scope: &self.execution_scope,
                ocomp_finality_authority: self.ocomp_finality_authority.clone(),
                ocomp_activation_block_meter: self.ocomp_activation_block_meter.clone(),
                ocomp_lifecycle_active: self.ocomp_lifecycle_active,
                ocomp_fork_install: None,
            },
        )? {
            return Ok(Some(result));
        }
        // Standard Ethereum precompiles `0x01..0x0a`; `Ok(None)` here lets the
        // caller push a real interpreter frame for ordinary contract targets.
        <EthPrecompiles as PrecompileProvider<&mut EthEvmContext<DB>>>::run(
            &mut self.eth,
            context,
            inputs,
        )
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        self.eth.warm_addresses()
    }

    fn contains(&self, address: &Address) -> bool {
        self.eth.contains(address)
    }
}

#[cfg(test)]
mod lysis_activation_entitlement_tests {
    use super::is_lysis_result_vote_call;
    use alloy_primitives::{Address, U256};
    use outbe_ocomp_protocol::abi::{
        GET_OFFCHAIN_JOB_SELECTOR, METADOSIS_ADDRESS, SUBMIT_LYSIS_RESULT_SELECTOR,
    };

    #[test]
    fn only_exact_non_static_value_free_metadosis_result_vote_is_entitled() {
        assert!(is_lysis_result_vote_call(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::ZERO,
        ));
        assert!(!is_lysis_result_vote_call(
            Address::repeat_byte(1),
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::ZERO,
        ));
        assert!(!is_lysis_result_vote_call(
            METADOSIS_ADDRESS,
            &GET_OFFCHAIN_JOB_SELECTOR,
            false,
            U256::ZERO,
        ));
        assert!(!is_lysis_result_vote_call(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            true,
            U256::ZERO,
        ));
        assert!(!is_lysis_result_vote_call(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::from(1),
        ));
    }
}
