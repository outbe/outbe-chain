use outbe_primitives::block::BlockLifecycle;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use super::current_preloaded_system_tx_context;

pub(super) fn run_cycle_tick_at_activation(
    ctx: &BlockRuntimeContext,
    metadosis_genesis_activation_height: u64,
) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    let lifecycle = outbe_cycle::lifecycle::CycleLifecycleContext::new(ctx.clone())
        .with_metadosis_genesis_activation_height(metadosis_genesis_activation_height);
    <outbe_cycle::lifecycle::CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)?;
    Ok(())
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
        // BoundaryOutcome, and a next-epoch validator proposed it. The
        // proposer becomes a consensus participant in BoundaryOutcome, at which
        // point `run_boundary_outcome` records this proposal exactly once.
    } else {
        return Err(
            crate::errors::BeginBlockPrecompileError::ProposerNotConsensusParticipant {
                proposer: ctx.block.proposer,
            }
            .into(),
        );
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

/// HookEvents system tx: no-op marker. The executor attaches whitelisted
/// pre-exec hook logs to this phase's receipt without re-running hooks.
pub(crate) fn run_hook_events(_ctx: &BlockRuntimeContext) -> Result<()> {
    Ok(())
}
