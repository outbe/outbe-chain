//! System-transaction orchestration precompile.
//!
//! The precompile is called through `transact_system_call` with
//! `SYSTEM_ADDRESS` as the EVM caller. It decodes a versioned
//! [`SystemTx`](crate::system_tx::SystemTx) payload and routes to
//! the system tx body. Thus runtime events are emitted through the
//! EVM journal and become receipt-visible.

use crate::system_tx::SystemTx;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::block::ProtocolModule as _;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

mod boundary;
mod context;
mod cycle;

#[cfg(test)]
pub(crate) use cycle::CYCLE_LATER_PHASE_RESERVE_GAS;

use context::block_runtime_context_from_storage;
#[cfg(test)]
pub(crate) use context::take_observed_runtime_block_contexts;
pub(crate) use context::{
    current_preloaded_system_tx_context, read_preloaded_finalized_summary,
    with_preloaded_system_tx_context, PreloadedSystemTxContext,
};

pub(crate) use crate::tee_bootstrap::execute_tee_bootstrap;

pub(crate) use crate::finalization::{run_finalization_and_slashing, run_late_finalize_credits};

pub use cycle::enforce_enclave_upgrade_deadline;
use cycle::run_cycle_tick;
pub(crate) use cycle::{
    run_hyperlane_liveness_window, run_oracle_slash_window, run_validator_liveness,
};

pub(crate) use boundary::run_boundary_outcome;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Dispatch entrypoint registered at [`OUTBE_SYSTEM_TX_ADDRESS`].
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        DispatchAuthorities {
            tee_attestation_v1:
                &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
        },
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
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        DispatchAuthorities { tee_attestation_v1 },
    )
}

/// Immutable chain-manifest authorities for system-transaction execution.
pub(crate) struct SystemTxRuntime<'a> {
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
    let SystemTxRuntime { tee_attestation_v1 } = runtime;
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        DispatchAuthorities { tee_attestation_v1 },
    )
}

type DispatchAuthorities<'a> = SystemTxRuntime<'a>;

fn dispatch_inner(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
    authorities: DispatchAuthorities<'_>,
) -> Result<Bytes> {
    let DispatchAuthorities { tee_attestation_v1 } = authorities;
    if caller != SYSTEM_ADDRESS {
        return Err(crate::errors::BeginBlockPrecompileError::InvalidSystemCaller.into());
    }
    if !value.is_zero() {
        return Err(crate::errors::BeginBlockPrecompileError::NativeValueRejected.into());
    }
    outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;

    let input = SystemTx::decode(data)
        .map_err(crate::errors::BeginBlockPrecompileError::invalid_system_tx_input)?;

    match input {
        SystemTx::CertifiedParentAccounting { metadata } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_finalization_and_slashing(&ctx, &metadata)?;
        }
        SystemTx::LateFinalizeCredits { artifact } => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_late_finalize_credits(&ctx, &artifact)?;
        }
        SystemTx::BoundaryOutcome { artifact } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_boundary_outcome(&ctx, &artifact)?;
        }
        SystemTx::TeeBootstrap { payload } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            execute_tee_bootstrap(&ctx, &payload, tee_attestation_v1)?;
        }
        SystemTx::ValidatorLiveness => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_validator_liveness(&ctx)?;
        }
        SystemTx::RadiclePublication { batch } => {
            outbe_coderegistry::CodeRegistry::new(storage).apply_publication_batch(&batch)?;
        }
        SystemTx::Vote => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            outbe_vote::VoteModule::begin_block(&ctx)?;
        }
        SystemTx::Update => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_update::UpdateModule::begin_block(&ctx)?;
        }
        SystemTx::EnclaveUpgradeDeadline => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            enforce_enclave_upgrade_deadline(&ctx)?;
        }
        SystemTx::Rewards => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_rewards::RewardsModule::begin_block(&ctx)?;
        }
        SystemTx::Staking => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_staking::StakingModule::begin_block(&ctx)?;
        }
        SystemTx::Oracle => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            outbe_oracle::OracleModule::begin_block(&ctx)?;
        }
        SystemTx::OracleSlash => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_oracle_slash_window(&ctx)?;
        }
        SystemTx::HyperlaneSlash => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_hyperlane_liveness_window(&ctx)?;
        }
        SystemTx::CycleTick => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_cycle_tick(&ctx, 1)?;
        }
        SystemTx::NodFactory => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_nodfactory::NodFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemFactory => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_gemfactory::GemFactoryModule::end_block(&ctx)?;
        }
        SystemTx::IntexFactory => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_intexfactory::IntexFactoryModule::end_block(&ctx)?;
        }
        SystemTx::CredisFactory => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_credisfactory::CredisFactoryModule::end_block(&ctx)?;
        }
        SystemTx::GemCleanup => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_gemfactory::GemCleanupModule::end_block(&ctx)?;
        }
    }

    Ok(Bytes::new())
}

#[cfg(test)]
mod tests;
