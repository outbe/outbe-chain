//! System-transaction orchestration precompile.
//!
//! The precompile is called through `transact_system_call` with
//! `SYSTEM_ADDRESS` as the EVM caller. It decodes a versioned
//! [`SystemTx`](crate::system_tx::SystemTx) payload and routes to
//! the system tx body. Thus runtime events are emitted through the
//! EVM journal and become receipt-visible.

#[cfg(feature = "test-utils")]
use crate::system_tx::SystemTx;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
#[cfg(feature = "test-utils")]
use outbe_primitives::addresses::SYSTEM_ADDRESS;
#[cfg(feature = "test-utils")]
use outbe_primitives::block::ProtocolModule;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

mod boundary;
mod context;
mod cycle;

#[cfg(feature = "test-utils")]
use context::block_runtime_context_from_storage;
pub use context::current_preloaded_system_tx_context;
#[cfg(test)]
pub(crate) use context::take_observed_runtime_block_contexts;
pub(crate) use context::{with_preloaded_system_tx_context, PreloadedSystemTxContext};

#[cfg(feature = "test-utils")]
pub(crate) use crate::tee_bootstrap::execute_tee_bootstrap;

#[cfg(feature = "test-utils")]
pub(crate) use crate::finalization::{run_finalization_and_slashing, run_late_finalize_credits};

#[cfg(feature = "test-utils")]
pub(crate) use cycle::run_validator_liveness;
pub use cycle::{enforce_enclave_upgrade_deadline, EnclaveUpgradeDeadline};

#[cfg(any(test, feature = "test-utils"))]
pub(crate) use boundary::run_boundary_outcome;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

fn fallback_system_tx_host() -> std::sync::Arc<dyn crate::EvmRuntimeHost> {
    crate::host::default_runtime_host()
}

/// Dispatch entrypoint registered at [`OUTBE_SYSTEM_TX_ADDRESS`].
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    fallback_system_tx_host().dispatch_system_tx(
        storage,
        data,
        caller,
        value,
        &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
    )
}

/// Dispatches system work with the immutable TEE authority from ChainSpec.
/// Component execution verifies the same genesis-fixed authority as the node.
pub fn dispatch_with_tee_attestation(
    storage: StorageHandle,
    tee_attestation_v1: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    fallback_system_tx_host().dispatch_system_tx(storage, data, caller, value, tee_attestation_v1)
}

/// Immutable chain-manifest authorities for system-transaction execution.
pub(crate) struct SystemTxRuntime<'a> {
    pub(crate) host: &'a dyn crate::EvmRuntimeHost,
    pub(crate) tee_attestation_v1:
        &'a crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
}

pub(crate) fn dispatch_with_runtime(
    storage: StorageHandle,
    runtime: SystemTxRuntime<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    let SystemTxRuntime {
        host,
        tee_attestation_v1,
    } = runtime;
    host.dispatch_system_tx(storage, data, caller, value, tee_attestation_v1)
}

#[cfg(feature = "test-utils")]
pub(crate) fn dispatch_system_tx_route(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    let host = crate::test_support::DomainEvmHost::new();
    dispatch_system_tx_domain(
        storage,
        data,
        caller,
        value,
        SystemTxRuntime {
            host: &host,
            tee_attestation_v1:
                &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
        },
    )
}

#[cfg(feature = "test-utils")]
type DispatchAuthorities<'a> = SystemTxRuntime<'a>;

#[cfg(test)]
std::thread_local! {
    static FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT: std::cell::RefCell<Option<(Address, U256, U256)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_forced_dirty_system_precompile_revert<R>(
    address: Address,
    slot: U256,
    value: U256,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
    }

    FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| {
        *cell.borrow_mut() = Some((address, slot, value));
    });
    let _reset = Reset;
    f()
}

#[cfg(feature = "test-utils")]
pub(crate) fn dispatch_system_tx_domain(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
    authorities: DispatchAuthorities<'_>,
) -> Result<Bytes> {
    let DispatchAuthorities {
        host,
        tee_attestation_v1,
    } = authorities;
    if caller != SYSTEM_ADDRESS {
        return Err(crate::errors::BeginBlockPrecompileError::InvalidSystemCaller.into());
    }
    if !value.is_zero() {
        return Err(crate::errors::BeginBlockPrecompileError::NativeValueRejected.into());
    }
    outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;

    #[cfg(test)]
    if let Some((dirty_addr, dirty_slot, dirty_val)) =
        FORCE_DIRTY_SYSTEM_PRECOMPILE_REVERT.with(|cell| *cell.borrow())
    {
        storage.sstore(dirty_addr, dirty_slot, dirty_val)?;
        storage.emit_event(
            dirty_addr,
            alloy_primitives::LogData::new_unchecked(
                vec![alloy_primitives::keccak256("DirtySoftFailEvent()")],
                Bytes::from_static(b"dirty-soft-fail-log"),
            ),
        )?;
        return Err(outbe_primitives::error::PrecompileError::Revert(
            "forced dirty system precompile revert".into(),
        ));
    }

    let input = SystemTx::decode(data)
        .map_err(crate::errors::BeginBlockPrecompileError::invalid_system_tx_input)?;

    match input {
        SystemTx::CertifiedParentAccounting { metadata } => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            run_finalization_and_slashing(host, &ctx, &metadata)?;
        }
        SystemTx::LateFinalizeCredits { artifact } => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            run_late_finalize_credits(host, &ctx, &artifact)?;
        }
        SystemTx::BoundaryOutcome { artifact } => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            run_boundary_outcome(host, &ctx, &artifact)?;
        }
        SystemTx::TeeBootstrap { payload } => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            execute_tee_bootstrap(host, &ctx, &payload, tee_attestation_v1)?;
        }
        SystemTx::ValidatorLiveness => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            run_validator_liveness(host, &ctx)?;
        }
        SystemTx::RadiclePublication { batch } => {
            outbe_coderegistry::CodeRegistry::new(storage).apply_publication_batch(&batch)?;
        }
        SystemTx::Vote => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            outbe_vote::VoteModule::begin_block(&ctx)?;
        }
        SystemTx::Update => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_update::UpdateModule::begin_block(&ctx)?;
        }
        SystemTx::EnclaveUpgradeDeadline => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            cycle::EnclaveUpgradeDeadline::begin_block(host, &ctx)?;
        }
        SystemTx::Rewards => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_rewards::RewardsModule::begin_block(&ctx)?;
        }
        SystemTx::Staking => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_staking::StakingModule::begin_block(&ctx)?;
        }
        SystemTx::Oracle => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            outbe_oracle::OracleModule::begin_block(&ctx)?;
        }
        SystemTx::OracleSlash => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_oracle::OracleSlashModule::begin_block(&ctx)?;
        }
        SystemTx::HyperlaneSlash => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_hyperlanecontroller::HyperlaneModule::begin_block(&ctx)?;
        }
        SystemTx::CycleTick => {
            let ctx = block_runtime_context_from_storage(host, storage, true)?;
            outbe_cycle::CycleModule::begin_block(&ctx)?;
        }
        SystemTx::NodFactory => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_nodfactory::NodFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemFactory => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_gemfactory::GemFactoryModule::end_block(&ctx)?;
        }
        SystemTx::IntexFactory => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_intexfactory::IntexFactoryModule::end_block(&ctx)?;
        }
        SystemTx::CredisFactory => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_credisfactory::CredisFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemCleanup => {
            let ctx = block_runtime_context_from_storage(host, storage, false)?;
            outbe_gemfactory::GemCleanupModule::end_block(&ctx)?;
        }
    }

    Ok(Bytes::new())
}

#[cfg(test)]
mod tests;
