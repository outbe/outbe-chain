//! Uses the same Outbe dispatch for nested calls as for top-level calls.
use super::{
    dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime},
    OutbePrecompileExecutionContext, OutbePrecompileRuntime,
};
use crate::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, B256};
use core::{fmt::Debug, marker::PhantomData};
use revm::{
    handler::{EthPrecompiles, PrecompileProvider},
    interpreter::{CallInputs, InterpreterResult},
    primitives::hardfork::SpecId,
    Database,
};

/// Precompile provider for the borrow-mode sub-call `Evm`
/// (`CTX = &mut EthEvmContext<DB>`), used by [`crate::sub_call`].
///
/// Mirrors the top-level [`alloy_evm::precompiles::PrecompilesMap`] dispatch path.
/// Outbe stateful precompiles dispatch through [`outbe_ctx_dispatch`]. The
/// runtime inputs are not identical to a top-level call:
/// - the TEE attestation ChainSpec state comes from the sub-call execution
///   context. `CtxStorageProvider::sub_call` builds that context as `Unbound`.
///
/// Everything else
/// (Ethereum precompiles `0x01..0x0a`, ordinary contract calls) falls back to
/// the standard [`EthPrecompiles`].
pub(crate) struct OutbeSubCallPrecompiles<DB> {
    /// Fallback provider for the Ethereum precompiles `0x01..0x0a`.
    eth: EthPrecompiles,
    /// EVM spec id, forwarded to [`outbe_ctx_dispatch`].
    spec: SpecId,
    genesis_hash: B256,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,

    abort_bridge: super::ExecutionAbortBridge,

    _db: PhantomData<fn() -> DB>,
}

impl<DB> OutbeSubCallPrecompiles<DB> {
    pub(crate) fn new(
        execution_context: OutbePrecompileExecutionContext,
        runtime: OutbePrecompileRuntime,
    ) -> Self {
        let OutbePrecompileExecutionContext {
            spec,
            genesis_hash,
            tee_attestation_v1,
        } = execution_context;
        let OutbePrecompileRuntime { abort_bridge } = runtime;
        Self {
            eth: EthPrecompiles::new(spec),
            spec,
            genesis_hash,
            tee_attestation_v1,
            abort_bridge,
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
                abort_bridge: &self.abort_bridge,
                spec: self.spec,
                genesis_hash: self.genesis_hash,
                tee_attestation_v1: &self.tee_attestation_v1,
            },
        )
        .map_err(String::from)?
        {
            return Ok(Some(result));
        }
        // Standard Ethereum precompiles `0x01..0x0a`. `Ok(None)` here lets the
        // caller push a real interpreter frame for ordinary contract targets.
        <EthPrecompiles as PrecompileProvider<&mut EthEvmContext<DB>>>::run(
            &mut self.eth,
            context,
            inputs,
        )
    }

    fn warm_addresses(&self) -> &alloy_primitives::map::AddressSet {
        self.eth.warm_addresses()
    }

    fn contains(&self, address: &Address) -> bool {
        self.eth.contains(address)
    }
}
