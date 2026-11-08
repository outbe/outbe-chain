use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule as _};
use outbe_primitives::error::Result;

use super::current_preloaded_system_tx_context;

/// Fourfold headroom over the measured sum of later begin-phase maxima,
/// rounded up to one million gas. See executor/tests/receipts_and_gas/calibration.rs
/// and sys_tx_precompile/tests/publication.rs: boundary 57,151,402, Oracle
/// 340,578,700, TEE bootstrap 266,500, and publication 1,691,100 gross gas.
/// These are supported-workload measurements, not bounds on arbitrary bridge code.
pub(crate) const CYCLE_LATER_PHASE_RESERVE_GAS: u64 = 1_599_000_000;

pub(crate) fn run_validator_liveness(ctx: &BlockRuntimeContext) -> Result<()> {
    validate_and_record_cycle_proposer(ctx)?;
    enforce_tee_lease_deadlines(ctx)?;
    Ok(())
}

pub(super) fn run_cycle_tick(
    ctx: &BlockRuntimeContext,
    metadosis_genesis_activation_height: u64,
) -> Result<()> {
    let Some(preloaded) = current_preloaded_system_tx_context() else {
        return outbe_cycle::CycleModule::begin_block(ctx);
    };
    let insufficient = || outbe_cycle::errors::CycleError::InsufficientWorkCapacity;
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
/// validator set. The sweep is part of receipt-visible `ValidatorLiveness`, before any
/// user transaction in the block. Block 1 is excluded because its earlier
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
    let mut vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    if vs.is_consensus_participant(ctx.block.proposer)? {
        vs.record_proposer(ctx.block.proposer)?;
        Ok(())
    } else {
        Err(
            crate::errors::BeginBlockPrecompileError::ProposerNotConsensusParticipant {
                proposer: ctx.block.proposer,
            }
            .into(),
        )
    }
}

pub(crate) fn enforce_enclave_upgrade_deadline(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut registry = outbe_teeregistry::TeeRegistry::new(ctx.storage.clone());
    let Some(upgrade) = registry.upgrade_sweep_due_v1()? else {
        return Ok(());
    };
    let mut reports = Vec::new();
    ctx.with_checkpoint(|| {
        let slash_percent = outbe_slashindicator::schema::SlashIndicator::new(ctx.storage.clone())
            .slash_amount_percent()?;
        let mut validators = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        let active = validators.get_active_validators()?;
        for validator in active {
            let address = validator.validator_address;
            let updated = registry
                .validator_enclave_binding_v1(address)?
                .is_some_and(|binding| {
                    binding.policy_hash == upgrade.successor_policy_hash
                        && binding.mrenclave == upgrade.mrenclave
                });
            if updated || registry.upgrade_penalty_applied_v1(upgrade.proposal_id, address)? {
                continue;
            }
            if let Some(report) = validators.jail_validator_deferred(address)? {
                reports.push(report);
            }
            let slashed = outbe_staking::contract::Staking::new(ctx.storage.clone())
                .slash_stake(address, slash_percent)?;
            registry.mark_upgrade_penalty_v1(upgrade.proposal_id, address)?;
            registry.emit(
                outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradeMissedV1 {
                    proposalId: upgrade.proposal_id,
                    validator: address,
                    activationHeight: upgrade.activation_height,
                    requiredMrenclave: upgrade.mrenclave,
                    slashedAmount: slashed,
                },
            )?;
        }
        registry.mark_upgrade_swept_v1(upgrade.proposal_id)
    })?;
    for report in reports {
        report.record();
    }
    Ok(())
}

/// OracleSlash system tx: run Oracle slash-window penalties after any
/// same-block boundary activation and Oracle tally/S-curve.
pub(crate) fn run_oracle_slash_window(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_oracle::OracleSlashModule::begin_block(ctx)
}

/// HyperlaneSlash system tx: run Hyperlane liveness window.
pub(crate) fn run_hyperlane_liveness_window(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_hyperlanecontroller::HyperlaneModule::begin_block(ctx)
}
