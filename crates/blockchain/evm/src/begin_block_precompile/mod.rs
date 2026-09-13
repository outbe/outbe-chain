//! Begin-block orchestration precompile system transactions.
//!
//! The precompile is called through `transact_system_call` with
//! `SYSTEM_ADDRESS` as the EVM caller. It decodes a versioned
//! [`SystemTxInputV2`](crate::system_tx::SystemTxInputV2) payload and routes to
//! the begin_block system tx body so runtime events are emitted through the
//! EVM journal and become receipt-visible.

use crate::system_tx::SystemTxInputV2;
use alloy_primitives::Address;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::error::PrecompileError;
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

use cycle::{run_cycle_tick_at_activation, run_cycle_tick_with_readers_at_activation};
pub(crate) use cycle::{
    run_hook_events, run_ocomp_lifecycle_begin, run_ocomp_terminal_request, run_oracle_slash_window,
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
        None,
        None,
        &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
    )
}

/// Dispatches begin-block work with explicit read-only body authority.
pub fn dispatch_with_readers(
    storage: StorageHandle,
    scope: &outbe_compressed_entities::ExecutionScope,
    parent: &outbe_offchain_data::RuntimeBodyReaders,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        Some((scope, parent)),
        None,
        &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
    )
}

/// Dispatches begin-block work without body readers while preserving the
/// immutable TEE authority derived from ChainSpec. Offline/component execution
/// does not install [`RuntimeBodyReaders`], but it must verify block-1 OST3
/// against the same genesis-fixed authority as the live node.
pub fn dispatch_with_tee_attestation(
    storage: StorageHandle,
    tee_attestation_v1: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_inner(storage, data, caller, value, None, None, tee_attestation_v1)
}

/// Production dispatch with both finalized body readers and the immutable
/// chain-manifest fork authority.
pub(crate) struct SystemTxRuntime<'a> {
    pub(crate) scope: &'a outbe_compressed_entities::ExecutionScope,
    pub(crate) parent: &'a outbe_offchain_data::RuntimeBodyReaders,
    pub(crate) ocomp_fork_install: Option<&'a outbe_metadosis::config::OcompForkInstallV1>,
    pub(crate) tee_attestation_v1:
        &'a crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
}

pub(crate) fn dispatch_with_readers_and_ocomp_install(
    storage: StorageHandle,
    runtime: SystemTxRuntime<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    let SystemTxRuntime {
        scope,
        parent,
        ocomp_fork_install,
        tee_attestation_v1,
    } = runtime;
    dispatch_inner(
        storage,
        data,
        caller,
        value,
        Some((scope, parent)),
        ocomp_fork_install,
        tee_attestation_v1,
    )
}

fn dispatch_inner(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
    body_readers: Option<(
        &outbe_compressed_entities::ExecutionScope,
        &outbe_offchain_data::RuntimeBodyReaders,
    )>,
    ocomp_fork_install: Option<&outbe_metadosis::config::OcompForkInstallV1>,
    tee_attestation_v1: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
) -> Result<Bytes> {
    if caller != SYSTEM_ADDRESS {
        return Err(PrecompileError::Revert(
            "system precompile can only be called by SYSTEM_ADDRESS".into(),
        ));
    }
    if !value.is_zero() {
        return Err(PrecompileError::Revert(
            "system precompile does not accept native token value".into(),
        ));
    }

    let input = SystemTxInputV2::decode(data)
        .map_err(|error| PrecompileError::Fatal(format!("invalid system tx input: {error}")))?;

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
            let (scope, _) = body_readers.ok_or_else(|| {
                PrecompileError::Fatal(
                    "OcompLifecycleBegin requires the active compressed-entity scope".into(),
                )
            })?;
            run_ocomp_lifecycle_begin(&ctx, scope, ocomp_fork_install)?;
        }
        SystemTxInputV2::CycleTick => {
            let ctx = block_runtime_context_from_storage(storage, true)?;
            let metadosis_genesis_activation_height = ocomp_fork_install
                .map(|install| install.activation_height)
                .unwrap_or(1);
            match body_readers {
                Some((scope, parent)) => {
                    run_cycle_tick_with_readers_at_activation(
                        &ctx,
                        scope,
                        parent,
                        metadosis_genesis_activation_height,
                    )?;
                }
                None => {
                    run_cycle_tick_at_activation(&ctx, metadosis_genesis_activation_height)?;
                }
            }
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
        }
        SystemTxInputV2::HookEvents => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            run_hook_events(&ctx)?;
        }
        SystemTxInputV2::OcompTerminalRequest => {
            let ctx = block_runtime_context_from_storage(storage, false)?;
            let (scope, _) = body_readers.ok_or_else(|| {
                PrecompileError::Fatal(
                    "OCOMP terminal request requires execution seal authority".into(),
                )
            })?;
            run_ocomp_terminal_request(&ctx, scope)?;
        }
    }

    Ok(Bytes::new())
}

#[cfg(test)]
mod tests;
