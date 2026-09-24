use outbe_primitives::block::BlockLifecycle;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;

use super::current_preloaded_system_tx_context;

pub(super) fn run_cycle_tick_at_activation(
    ctx: &BlockRuntimeContext,
    _metadosis_genesis_activation_height: u64,
) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;

    #[cfg(test)]
    {
        use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
        use std::sync::Arc;

        let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
        let parent = outbe_offchain_data::RuntimeBodyReaders::new(storage);
        let scope = outbe_compressed_entities::ExecutionScope::new();
        let compressed =
            outbe_compressed_entities::CompressedEntitiesLifecycleContext::new(ctx.clone(), &scope);
        <outbe_compressed_entities::CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(
            &compressed,
        )?;
        let lifecycle =
            outbe_cycle::lifecycle::CycleLifecycleContext::new(ctx.clone(), &scope, &parent)
                .with_metadosis_genesis_activation_height(_metadosis_genesis_activation_height);
        <outbe_cycle::lifecycle::CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)?;
        <outbe_compressed_entities::CompressedEntitiesLifecycle as BlockLifecycle>::end_block(
            &compressed,
        )
        .map(|_| ())?;
        run_ocomp_recovery_sweep(ctx)
    }

    #[cfg(not(test))]
    Err(PrecompileError::Fatal(
        "Cycle execution body read authority was not supplied".into(),
    ))
}

pub(super) fn run_cycle_tick_with_readers_at_activation(
    ctx: &BlockRuntimeContext,
    scope: &outbe_compressed_entities::ExecutionScope,
    parent: &outbe_offchain_data::RuntimeBodyReaders,
    metadosis_genesis_activation_height: u64,
) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    let cycle_lifecycle =
        outbe_cycle::lifecycle::CycleLifecycleContext::new(ctx.clone(), scope, parent)
            .with_metadosis_genesis_activation_height(metadosis_genesis_activation_height);
    <outbe_cycle::lifecycle::CycleLifecycle as BlockLifecycle>::begin_block(&cycle_lifecycle)?;
    // Keep non-transactional punishment observability behind every other
    // fallible CycleTick lifecycle. Once this succeeds, the precompile returns
    // immediately and the critical system transaction can commit as one unit.
    run_ocomp_recovery_sweep(ctx)
}

/// Resolve due OCOMP recovery windows in the mandatory per-block `CycleTick`.
/// `CycleTick` is consensus-critical, so every accepted block at height >= 1
/// has executed this sweep successfully.
pub(super) fn run_ocomp_recovery_sweep(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_staking::contract::Staking::new(ctx.storage.clone())
        .close_due_ocomp_recovery_windows()
        .map(|_| ())
}

/// Applies the canonical post-bootstrap TEE lease deadline to the bounded ACTIVE
/// validator set. The sweep is part of receipt-visible `CycleTick`, before any
/// user transaction in the block. Block 1 is excluded because its later
/// `TeeBootstrap` system transaction creates the founder bindings atomically.
pub(super) fn enforce_tee_lease_deadlines(ctx: &BlockRuntimeContext) -> Result<usize> {
    if ctx.block.block_number
        <= crate::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT
    {
        return Ok(0);
    }

    let active = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
        .get_active_validators()?;
    let registry = outbe_teeregistry::TeeRegistry::new(ctx.storage.clone());
    let mut overdue = Vec::new();
    for validator in active {
        let binding = registry.validator_enclave_binding_v1(validator.validator_address)?;
        if binding.is_none_or(|binding| binding.valid_until <= ctx.block.timestamp) {
            overdue.push(validator.validator_address);
        }
    }

    let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    let mut jailed = 0usize;
    for validator in overdue {
        if validator_set.jail_validator_for_tee_expiry(validator)? {
            jailed += 1;
        }
    }
    Ok(jailed)
}

fn validate_and_record_cycle_proposer(ctx: &BlockRuntimeContext) -> Result<()> {
    let allow_boundary_proposer = current_preloaded_system_tx_context()
        .map(|context| context.allow_boundary_proposer)
        .unwrap_or(false);
    let mut vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    if vs.is_consensus_participant(ctx.block.proposer)? {
        vs.record_proposer(ctx.block.proposer)?;
    } else if allow_boundary_proposer && vs.is_validator(ctx.block.proposer)? {
        // The block is the activation block for a validator-set-changing
        // BoundaryOutcome and was proposed by a next-epoch validator. The
        // proposer becomes a consensus participant in BoundaryOutcome, at which
        // point `run_boundary_outcome` records this proposal exactly once.
    } else {
        return Err(PrecompileError::Fatal(format!(
            "proposer is not a current consensus participant: {}",
            ctx.block.proposer
        )));
    }
    Ok(())
}

/// OracleSlashWindow system tx: run Oracle slash-window penalties after any
/// same-block boundary activation but before user transactions observe state.
pub(crate) fn run_oracle_slash_window(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_oracle::lifecycle::run_slash_window(ctx)
}

/// Hyperlane liveness window, same phase as the Oracle slash window.
pub(crate) fn run_hyperlane_liveness_window(ctx: &BlockRuntimeContext) {
    outbe_hyperlanecontroller::lifecycle::run_liveness_window(ctx)
}

/// HookEvents system tx: no-op marker. Whitelisted pre-exec hook logs are
/// attached to this phase's receipt by the executor without re-running hooks.
pub(crate) fn run_hook_events(_ctx: &BlockRuntimeContext) -> Result<()> {
    Ok(())
}

/// Reserved OCOMP expiry/reset slot. OCM-08 wires the bounded Metadosis
/// lifecycle handler into this already receipt-visible phase.
pub(crate) fn run_ocomp_lifecycle_begin(
    ctx: &BlockRuntimeContext,
    scope: &outbe_compressed_entities::ExecutionScope,
    fork_install: Option<&outbe_metadosis::config::OcompForkInstallV1>,
) -> Result<()> {
    if let Some(install) = fork_install {
        if ctx.block.block_number == install.activation_height {
            outbe_metadosis::commands::install_fork_profile(ctx, install)?;
        }
    }
    outbe_metadosis::commands::run_ocomp_lifecycle_begin_with_scope(ctx, scope)
}

/// Reserved terminal request slot. It consumes executor-owned provisional CE
/// roots while the scope remains active for terminal failure retirement.
pub(crate) fn run_ocomp_terminal_request(
    ctx: &BlockRuntimeContext,
    scope: &outbe_compressed_entities::ExecutionScope,
) -> Result<()> {
    outbe_metadosis::commands::run_ocomp_terminal_request(ctx, scope)
}
