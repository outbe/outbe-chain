use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use super::current_preloaded_system_tx_context;

/// Fourfold headroom over the measured sum of later begin-phase maxima,
/// rounded up to one million gas. See executor/tests/receipts_and_gas/calibration.rs
/// and sys_tx_precompile/tests/publication.rs: boundary 57,151,402, Oracle
/// 340,578,700, TEE bootstrap 266,500, and publication 1,691,100 gross gas.
/// These are supported-workload measurements, not bounds on arbitrary bridge code.
pub(crate) const CYCLE_LATER_PHASE_RESERVE_GAS: u64 = 1_599_000_000;

pub(super) fn run_cycle_tick(
    ctx: &BlockRuntimeContext,
    metadosis_genesis_activation_height: u64,
) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    let insufficient = || outbe_cycle::errors::CycleError::InsufficientWorkCapacity;
    let preloaded = current_preloaded_system_tx_context().ok_or_else(insufficient)?;
    let remaining = ctx
        .storage
        .metered_gas_remaining()?
        .ok_or_else(insufficient)?;
    // Begin-zone calls execute with this internal allowance, not their signed
    // visible-gas limit. The difference includes the precompile base and all
    // proposer/TEE prefix work, so setup is deducted exactly once.
    let call_limit = preloaded
        .gas_limit_override
        .unwrap_or(outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    let spent_before_body = call_limit.checked_sub(remaining).ok_or_else(insufficient)?;
    let capacity = outbe_cycle::runtime::CycleWorkCapacity {
        remaining_gas: preloaded
            .remaining_system_gross_gas
            .checked_sub(spent_before_body)
            .ok_or_else(insufficient)?,
        gas_used_at_entry: ctx.storage.gas_used()?,
        reserve_after_cycle: CYCLE_LATER_PHASE_RESERVE_GAS,
    };
    outbe_cycle::runtime::handle_cycle_tick(ctx, metadosis_genesis_activation_height, capacity)
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
    if overdue.is_empty() {
        return Ok(0);
    }

    // Admit the percent and the clock before any jail. One checkpoint holds
    // every jail, slash, and log. A later failure rolls the whole sweep back.
    let transitioned = ctx.storage.with_checkpoint(|| {
        let percent = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone())
            .slash_amount_percent()?;
        ctx.storage.now_secs()?;
        let mut validator_set =
            outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        let mut staking = outbe_staking::contract::Staking::new(ctx.storage.clone());
        let mut transitioned = Vec::new();
        for validator in overdue {
            if validator_set.jail_validator_for_tee_expiry(validator)? {
                staking.slash_stake(validator, percent)?;
                transitioned.push(validator);
            }
        }
        Ok(transitioned)
    })?;

    for validator in &transitioned {
        outbe_validatorset::metrics::record_validator_status(
            *validator,
            outbe_validatorset::runtime::status::JAILED,
        );
        outbe_validatorset::metrics::record_validator_tee_expiry(*validator, "deadline_jailed");
        outbe_validatorset::metrics::record_pending_set_change(true);
        tracing::warn!(
            target: "outbe::validatorset",
            event = "validator_tee_lease_expired",
            validator = %validator,
            block_number = ctx.block.block_number,
            "validator jailed and slashed after TEE lease deadline",
        );
    }
    Ok(transitioned.len())
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
