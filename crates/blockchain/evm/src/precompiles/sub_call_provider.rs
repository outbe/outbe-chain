//! Uses the same Outbe dispatch for nested calls as for top-level calls.
use super::{
    dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime},
    OcompActivationBlockMeter, OutbePrecompileExecutionContext, OutbePrecompileRuntime,
};
use crate::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, B256};
use core::{fmt::Debug, marker::PhantomData};
use outbe_compressed_entities::ExecutionScope;
use outbe_metadosis::api::OcompFinalizedIntentAuthority;
use outbe_offchain_data::RuntimeBodyReaders;
use revm::{
    handler::{EthPrecompiles, PrecompileProvider},
    interpreter::{CallInputs, InterpreterResult},
    primitives::hardfork::SpecId,
    Database,
};
use std::sync::Arc;

/// Precompile provider for the borrow-mode sub-call `Evm`
/// (`CTX = &mut EthEvmContext<DB>`), used by [`crate::sub_call`].
///
/// Mirrors the top-level [`alloy_evm::precompiles::PrecompilesMap`] semantics, so a sub-call to
/// any outbe precompile behaves exactly like a top-level call. Outbe stateful
/// precompiles dispatch through [`outbe_ctx_dispatch`]. Everything else
/// (Ethereum precompiles `0x01..0x0a`, ordinary contract calls) falls back to
/// the standard [`EthPrecompiles`].
pub(crate) struct OutbeSubCallPrecompiles<DB> {
    /// Fallback provider for the Ethereum precompiles `0x01..0x0a`.
    eth: EthPrecompiles,
    /// EVM spec id, forwarded to [`outbe_ctx_dispatch`].
    spec: SpecId,
    genesis_hash: B256,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,
    runtime_body_readers: Option<RuntimeBodyReaders>,
    execution_scope: Arc<ExecutionScope>,
    ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
    ocomp_lifecycle_active: bool,
    _db: PhantomData<fn() -> DB>,
}

impl<DB> OutbeSubCallPrecompiles<DB> {
    pub(crate) fn new(
        execution_context: OutbePrecompileExecutionContext,
        runtime: OutbePrecompileRuntime,
        ocomp_activation_block_meter: Arc<OcompActivationBlockMeter>,
    ) -> Self {
        let OutbePrecompileExecutionContext {
            spec,
            genesis_hash,
            tee_attestation_v1,
        } = execution_context;
        let OutbePrecompileRuntime {
            runtime_body_readers,
            execution_scope,
            ocomp_finality_authority,
            ocomp_lifecycle_active,
        } = runtime;
        Self {
            eth: EthPrecompiles::new(spec),
            spec,
            genesis_hash,
            tee_attestation_v1,
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
                genesis_hash: self.genesis_hash,
                tee_attestation_v1: &self.tee_attestation_v1,
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
