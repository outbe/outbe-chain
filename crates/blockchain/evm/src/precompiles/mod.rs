//! Registers Outbe precompiles and preserves their public execution interface.
//! This facade keeps domain admission and EVM outcome translation behind it.

use crate::{factory::OutbeEvmTypes, tee_attestation_activation::TeeAttestationChainSpecStateV1};
use alloy_primitives::{Address, B256};
use dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime};
use evm2::{
    evm::{precompile::PrecompileProvider, PrewarmSet},
    interpreter::{GasTracker, Message},
    precompiles::{MovePrecompileError, PrecompileId, PrecompileResult},
    Evm, Precompiles, SpecId,
};
use std::sync::Arc;

mod call_authority;
pub(crate) mod routes;
pub use call_authority::PrecompileMutationCall;
mod cancellation;
pub use cancellation::ExecutionAbortBridge;
pub(crate) use cancellation::{preserve_cancelled_execution, SourceVisibleAbort};
mod dispatch;
mod outcome;
mod sub_call_provider;
#[cfg(test)]
mod tests;
mod value_policy;

pub use outcome::map_outbe_precompile_result;
pub use routes::assert_valid_production_routes;

/// Immutable protocol context shared by every Outbe precompile dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutbePrecompileExecutionContext {
    spec: SpecId,
    genesis_hash: B256,
    genesis_timestamp: u64,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,
}

impl OutbePrecompileExecutionContext {
    #[must_use]
    pub const fn new(spec: SpecId, genesis_hash: B256) -> Self {
        Self {
            spec,
            genesis_hash,
            genesis_timestamp: 0,
            tee_attestation_v1: TeeAttestationChainSpecStateV1::Unbound,
        }
    }

    pub const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }

    pub(crate) const fn spec_id(&self) -> SpecId {
        self.spec
    }

    #[must_use]
    pub fn with_tee_attestation_v1(mut self, state: TeeAttestationChainSpecStateV1) -> Self {
        self.tee_attestation_v1 = state;
        self
    }
}

#[derive(Clone, Default)]
pub struct OutbePrecompileRuntime {
    pub(crate) abort_bridge: ExecutionAbortBridge,
}

impl OutbePrecompileRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_abort_bridge(mut self, bridge: ExecutionAbortBridge) -> Self {
        self.abort_bridge = bridge;
        self
    }
}

/// Safe `evm2` precompile provider that dispatches Outbe stateful routes directly
/// against `&mut evm2::Evm<'_, OutbeEvmTypes>` and delegates all other addresses
/// to the standard Ethereum precompile table.
#[derive(Clone)]
pub struct OutbePrecompileProvider {
    inner: Precompiles<OutbeEvmTypes>,
    execution_context: OutbePrecompileExecutionContext,
    runtime: OutbePrecompileRuntime,
    host: Arc<dyn crate::EvmRuntimeHost>,
}

impl std::fmt::Debug for OutbePrecompileProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbePrecompileProvider")
            .field("execution_context", &self.execution_context)
            .field("host", &"installed")
            .finish()
    }
}

impl OutbePrecompileProvider {
    /// Builds a new safe precompile provider combining standard Ethereum
    /// precompiles for `execution_context.spec_id()` with Outbe stateful routes.
    #[must_use]
    pub fn new(
        execution_context: OutbePrecompileExecutionContext,
        runtime: OutbePrecompileRuntime,
        host: Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self {
            inner: Precompiles::base(execution_context.spec_id()),
            execution_context,
            runtime,
            host,
        }
    }
}

impl PrecompileProvider<OutbeEvmTypes> for OutbePrecompileProvider {
    fn contains(&self, address: &Address) -> bool {
        self.host.is_registered_precompile(address) || self.inner.contains(address)
    }

    fn addresses(&self) -> Vec<Address> {
        self.inner.addresses()
    }

    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        self.inner.precompile_ids()
    }

    fn warm_addresses(&self, warm: &mut PrewarmSet) {
        self.inner.warm_addresses(warm);
    }

    fn execute(
        &mut self,
        evm: &mut Evm<'_, OutbeEvmTypes>,
        message: &Message<OutbeEvmTypes>,
        gas: &mut GasTracker,
    ) -> Option<PrecompileResult> {
        if self.host.is_registered_precompile(&message.code_address) {
            return outbe_ctx_dispatch(
                evm,
                message,
                gas,
                OutbeDispatchRuntime {
                    abort_bridge: &self.runtime.abort_bridge,
                    spec: self.execution_context.spec,
                    genesis_hash: self.execution_context.genesis_hash,
                    genesis_timestamp: self.execution_context.genesis_timestamp,
                    tee_attestation_v1: &self.execution_context.tee_attestation_v1,
                    host: Arc::clone(&self.host),
                },
            );
        }
        self.inner.execute(evm, message, gas)
    }

    fn move_precompiles(
        &mut self,
        moves: &[(Address, Address)],
    ) -> Result<(), MovePrecompileError> {
        self.inner.move_precompiles(moves)
    }
}
