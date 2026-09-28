//! Registers Outbe precompiles and preserves their public execution interface.
//! Domain admission and EVM outcome translation are kept behind this facade.

use crate::{precompile_routes, tee_attestation_activation::TeeAttestationChainSpecStateV1};
use alloy_evm::{eth::EthEvmContext, precompiles::PrecompilesMap};
use alloy_primitives::{Address, B256};
use core::fmt::Debug;
use dispatch::{outbe_ctx_dispatch, OutbeDispatchRuntime};
use outbe_compressed_entities::ExecutionScope;
use outbe_metadosis::{api::OcompFinalizedIntentAuthority, config::OcompForkInstallV1};
use outbe_offchain_data::RuntimeBodyReaders;
use revm::{primitives::hardfork::SpecId, Database};
use std::sync::Arc;

mod call_authority;
mod dispatch;
mod outcome;
mod sub_call_provider;
#[cfg(test)]
mod tests;
mod value_policy;

pub use outcome::map_outbe_precompile_result;
pub(crate) use sub_call_provider::OutbeSubCallPrecompiles;

/// Shared marker retained in the sub-call context while q-forming apply
/// accounting is migrated to the direct result-vote path.
#[derive(Debug, Default)]
pub struct OcompActivationBlockMeter;
/// Returns the list of outbe precompile addresses registered by
/// [`extend_outbe_precompiles`].
///
/// Lookup and enumeration are generated from the same compact declaration in
/// [`crate::precompile_routes`], so dispatch-recognized exact routes cannot be omitted
/// from this list.
pub fn outbe_precompile_addresses() -> &'static [Address] {
    precompile_routes::EXACT_ADDRESSES
}

/// Immutable protocol context shared by every Outbe precompile dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutbePrecompileExecutionContext {
    spec: SpecId,
    genesis_hash: B256,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,
}

impl OutbePrecompileExecutionContext {
    #[must_use]
    pub const fn new(spec: SpecId, genesis_hash: B256) -> Self {
        Self {
            spec,
            genesis_hash,
            tee_attestation_v1: TeeAttestationChainSpecStateV1::Unbound,
        }
    }

    #[must_use]
    pub fn with_tee_attestation_v1(mut self, state: TeeAttestationChainSpecStateV1) -> Self {
        self.tee_attestation_v1 = state;
        self
    }
}

#[derive(Clone)]
pub struct OutbePrecompileRuntime {
    runtime_body_readers: Option<RuntimeBodyReaders>,
    execution_scope: Arc<ExecutionScope>,
    ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
    ocomp_lifecycle_active: bool,
}

impl OutbePrecompileRuntime {
    pub fn new(
        runtime_body_readers: Option<RuntimeBodyReaders>,
        execution_scope: Arc<ExecutionScope>,
        ocomp_finality_authority: Option<Arc<dyn OcompFinalizedIntentAuthority>>,
        ocomp_lifecycle_active: bool,
    ) -> Self {
        Self {
            runtime_body_readers,
            execution_scope,
            ocomp_finality_authority,
            ocomp_lifecycle_active,
        }
    }
}

/// Register outbe stateful precompile dispatch on the given [`PrecompilesMap`]
/// via the `set_ctx_dispatch_hook` fork extension.
///
/// The hook receives a raw pointer to the unbroken `&mut EthEvmContext<DB>`
/// before revm destructures it into `EvmInternals`. The dispatch closure
/// casts the pointer back to `&mut EthEvmContext<DB>` (safe because the
/// `EvmFactory` impl that called us is specialised for the same DB), builds a
/// [`crate::storage::CtxStorageProvider`] borrowing that context, and dispatches the outbe
/// precompile through a [`StorageHandle`]. Sub-call from precompile body
/// reaches `sub_call::run` through the provider's `sub_call` method.
/// Registers Outbe precompiles with an executor-owned compressed-entity scope.
pub fn extend_outbe_precompiles<DB>(
    precompiles: &mut PrecompilesMap,
    execution_context: OutbePrecompileExecutionContext,
    runtime: OutbePrecompileRuntime,
    ocomp_fork_install: Option<Arc<OcompForkInstallV1>>,
) where
    DB: Database + Debug,
    DB::Error: Debug,
{
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
                    genesis_hash,
                    tee_attestation_v1: &tee_attestation_v1,
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
