//! Begin-block orchestration precompile system transactions.
//!
//! The precompile is called through `transact_system_call` with
//! `SYSTEM_ADDRESS` as the EVM caller. It decodes a versioned
//! [`SystemTxInputV2`](crate::system_tx::SystemTxInputV2) payload and routes to
//! the begin_block system tx body. Thus runtime events are emitted through the
//! EVM journal and become receipt-visible.

use crate::system_tx::SystemTxInputV2;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

mod boundary;
mod context;
mod cycle;
mod finalization;
mod late_credits;
mod tee_bootstrap;

pub(super) use context::preloaded_certified_parent_state_root;
use context::{
    block_runtime_context_from_storage, current_preloaded_system_tx_context,
    read_preloaded_finalized_summary,
};
pub(crate) use context::{with_preloaded_system_tx_context, PreloadedSystemTxContext};

use tee_bootstrap::prepare_tee_bootstrap;
pub(crate) use tee_bootstrap::run_tee_bootstrap_v1;

pub(crate) use finalization::run_finalization_and_slashing;

// Preserve the existing crate-visible path; its callers now live in the same leaf.
#[allow(unused_imports)]
pub(crate) use late_credits::authenticate_late_credit;
pub(crate) use late_credits::run_late_finalize_credits;

use cycle::run_cycle_tick_at_activation;
pub(crate) use cycle::{
    run_hook_events, run_hyperlane_liveness_window, run_ocomp_lifecycle_begin,
    run_ocomp_terminal_request, run_oracle_slash_window,
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
            ocomp_fork_install: None,
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
        DispatchAuthorities {
            ocomp_fork_install: None,
            tee_attestation_v1,
        },
    )
}

/// Immutable chain-manifest authorities for system-transaction execution.
pub(crate) struct SystemTxRuntime<'a> {
    pub(crate) ocomp_fork_install: Option<&'a outbe_metadosis::config::OcompForkInstallV1>,
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
        ocomp_fork_install,
        tee_attestation_v1,
    } = runtime;
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        DispatchAuthorities {
            ocomp_fork_install,
            tee_attestation_v1,
        },
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
    let DispatchAuthorities {
        ocomp_fork_install,
        tee_attestation_v1,
    } = authorities;
    if caller != SYSTEM_ADDRESS {
        return Err(crate::errors::BeginBlockPrecompileError::InvalidSystemCaller.into());
    }
    if !value.is_zero() {
        return Err(crate::errors::BeginBlockPrecompileError::NativeValueRejected.into());
    }
    outbe_primitives::dispatch::ensure_mutation_allowed(&storage)?;

    let input = SystemTxInputV2::decode(data)
        .map_err(crate::errors::BeginBlockPrecompileError::invalid_system_tx_input)?;

    match input {
        SystemTxInputV2::CertifiedParentAccounting { metadata } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_finalization_and_slashing(&ctx, &metadata)?;
        }
        SystemTxInputV2::LateFinalizeCredits { artifact } => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_late_finalize_credits(&ctx, &artifact)?;
        }
        SystemTxInputV2::OcompLifecycleBegin => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_ocomp_lifecycle_begin(&ctx, ocomp_fork_install)?;
        }
        SystemTxInputV2::CycleTick => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            let metadosis_genesis_activation_height = ocomp_fork_install
                .map(|install| install.activation_height)
                .unwrap_or(1);
            run_cycle_tick_at_activation(&ctx, metadosis_genesis_activation_height)?;
        }
        SystemTxInputV2::RewardsGemDelivery => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            outbe_rewards::api::deliver_oldest_reward_gem_batch(&ctx)?;
        }
        SystemTxInputV2::BoundaryOutcome { artifact } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            run_boundary_outcome(&ctx, &artifact)?;
        }
        SystemTxInputV2::TeeBootstrap { payload } => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            prepare_tee_bootstrap(&ctx, &payload, tee_attestation_v1)?;
            run_tee_bootstrap_v1(&ctx, &payload)?;
        }
        SystemTxInputV2::OracleSlashWindow => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_oracle_slash_window(&ctx)?;
            run_hyperlane_liveness_window(&ctx);
        }
        SystemTxInputV2::HookEvents => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_hook_events(&ctx)?;
        }
        SystemTxInputV2::RadiclePublication { batch } => {
            outbe_radicleregistry::RadicleRegistry::new(storage).apply_publication_batch(&batch)?;
        }
        SystemTxInputV2::OcompTerminalRequest => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_ocomp_terminal_request(&ctx)?;
        }
    }

    Ok(Bytes::new())
}

#[cfg(test)]
mod tests;
